// churns files on a refs volume as fetchloom does, one way per run, to find which way deadlocks
// refs. usage: refs-stress <arm> <root> <threads> <seconds> <progress file>
// arms: plain (write, delete), link (write, hard link, delete), rename (write, rename without
// replacing), clone (write, block clone, delete), fix (publish by rename, place by clone and rename),
// seal-link and seal-fix (as link and fix, each file sealed as fetchloom seals an object: written,
// dated, flushed and read-only, and removed by a posix delete that ignores read-only), tree-link and
// tree-fix (each pass a new small tree, as a test's sandbox: a probe link, then sealed files published
// into objects/ and placed in data/, by links or by rename and clone, then the tree removed)
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.Win32.SafeHandles;

static class RefsStress
{
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateHardLinkW(string link, string existing, IntPtr security);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool MoveFileExW(string from, string to, uint flags);

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetFileInformationByHandle(SafeFileHandle file, int type, ref uint flags, int size);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetFileTime(SafeFileHandle file, IntPtr created, IntPtr accessed, ref long written);

    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(SafeFileHandle device, uint code, IntPtr input, int inputSize,
        IntPtr output, int outputSize, out int returned, IntPtr overlapped);

    [StructLayout(LayoutKind.Sequential)]
    struct DuplicateExtents
    {
        public IntPtr FileHandle;
        public long SourceFileOffset;
        public long TargetFileOffset;
        public long ByteCount;
    }

    const uint FSCTL_SET_SPARSE = 0x000900c4;
    const uint FSCTL_GET_INTEGRITY_INFORMATION = 0x0009027c;
    const uint FSCTL_SET_INTEGRITY_INFORMATION = 0x0009c280;
    const uint FSCTL_DUPLICATE_EXTENTS_TO_FILE = 0x00098344;
    const int Folders = 4;
    const int Kept = 64;

    static string arm;
    static string root;
    static volatile bool stop;
    static long[] last;
    static long ops;
    static string failure;

    static void Check(bool ok, string what)
    {
        if (!ok) throw new Win32Exception(Marshal.GetLastWin32Error(), what);
    }

    static void Control(SafeFileHandle file, uint code, byte[] input, byte[] output, string what)
    {
        GCHandle inPin = GCHandle.Alloc(input ?? new byte[0], GCHandleType.Pinned);
        GCHandle outPin = GCHandle.Alloc(output ?? new byte[0], GCHandleType.Pinned);
        try
        {
            int returned;
            Check(DeviceIoControl(file, code, input == null ? IntPtr.Zero : inPin.AddrOfPinnedObject(), input == null ? 0 : input.Length,
                output == null ? IntPtr.Zero : outPin.AddrOfPinnedObject(), output == null ? 0 : output.Length, out returned, IntPtr.Zero), what);
        }
        finally
        {
            inPin.Free();
            outPin.Free();
        }
    }

    // as reflink-copy does it: a sparse new file the source's length, its integrity copied, the
    // extents duplicated, then not sparse again
    static void Clone(string from, string to, long size)
    {
        using (FileStream source = new FileStream(from, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete))
        using (FileStream target = new FileStream(to, FileMode.CreateNew, FileAccess.ReadWrite, FileShare.None))
        {
            Control(target.SafeFileHandle, FSCTL_SET_SPARSE, null, null, "set sparse");
            byte[] integrity = new byte[16];
            Control(source.SafeFileHandle, FSCTL_GET_INTEGRITY_INFORMATION, null, integrity, "get integrity");
            byte[] set = new byte[8];
            Array.Copy(integrity, 0, set, 0, 8);
            try { Control(target.SafeFileHandle, FSCTL_SET_INTEGRITY_INFORMATION, set, null, "set integrity"); } catch (Win32Exception) { }
            target.SetLength(size);
            DuplicateExtents extents = new DuplicateExtents();
            extents.FileHandle = source.SafeFileHandle.DangerousGetHandle();
            extents.ByteCount = size;
            int length = Marshal.SizeOf(typeof(DuplicateExtents));
            byte[] input = new byte[length];
            GCHandle pin = GCHandle.Alloc(input, GCHandleType.Pinned);
            try { Marshal.StructureToPtr(extents, pin.AddrOfPinnedObject(), false); } finally { pin.Free(); }
            Control(target.SafeFileHandle, FSCTL_DUPLICATE_EXTENTS_TO_FILE, input, null, "duplicate extents");
            Control(target.SafeFileHandle, FSCTL_SET_SPARSE, new byte[] { 0 }, null, "unset sparse");
        }
    }

    static void Rename(string from, string to)
    {
        Check(MoveFileExW(from, to, 0), "rename " + from);
    }

    // as fetchloom seals an object: made read-only, written, dated just past its birth, flushed
    static void Seal(string path, byte[] bytes)
    {
        using (FileStream file = new FileStream(path, FileMode.CreateNew, FileAccess.ReadWrite, FileShare.Read))
        {
            file.Write(bytes, 0, bytes.Length);
            long written = DateTime.UtcNow.ToFileTimeUtc();
            Check(SetFileTime(file.SafeFileHandle, IntPtr.Zero, IntPtr.Zero, ref written), "date " + path);
            file.Flush(true);
        }
        File.SetAttributes(path, FileAttributes.ReadOnly);
    }

    // as rust's remove_file: a posix delete that ignores read-only, else a plain one
    static void Remove(string path)
    {
        const uint DELETE = 0x10000, SHARE_ALL = 7, OPEN_EXISTING = 3;
        const uint OPEN_REPARSE_POINT = 0x00200000, BACKUP_SEMANTICS = 0x02000000;
        using (SafeFileHandle file = CreateFileW(path, DELETE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING,
            OPEN_REPARSE_POINT | BACKUP_SEMANTICS, IntPtr.Zero))
        {
            Check(!file.IsInvalid, "open to delete " + path);
            uint posix = 0x1 | 0x2 | 0x10;
            if (SetFileInformationByHandle(file, 21, ref posix, 4)) return;
            uint plain = 0x1 | 0x10;
            Check(SetFileInformationByHandle(file, 21, ref plain, 4), "delete " + path);
        }
    }

    // one pass of a small tree, as a fetchloom command in a new sandbox makes and drops one
    static void Tree(string folder, byte[] bytes, long size, bool links)
    {
        string tmp = Path.Combine(folder, "tmp");
        string objects = Path.Combine(folder, "objects", "ab");
        string data = Path.Combine(folder, "data", "set");
        Directory.CreateDirectory(tmp);
        Directory.CreateDirectory(objects);
        Directory.CreateDirectory(data);
        if (links)
        {
            string probe = Path.Combine(tmp, ".probe");
            File.WriteAllBytes(probe, new byte[0]);
            File.SetAttributes(probe, FileAttributes.Hidden);
            string probed = Path.Combine(tmp, ".probe-link");
            Check(CreateHardLinkW(probed, probe, IntPtr.Zero), "link " + probed);
            Remove(probe);
            Remove(probed);
        }
        for (int file = 0; file < 4; file++)
        {
            string part = Path.Combine(tmp, file + ".part");
            string obj = Path.Combine(objects, "object" + file);
            string placed = Path.Combine(data, file + ".csv");
            Seal(part, bytes);
            if (links)
            {
                Check(CreateHardLinkW(obj, part, IntPtr.Zero), "link " + obj);
                Remove(part);
                Check(CreateHardLinkW(placed, obj, IntPtr.Zero), "link " + placed);
            }
            else
            {
                File.SetAttributes(part, FileAttributes.Normal);
                Rename(part, obj);
                File.SetAttributes(obj, FileAttributes.ReadOnly);
                string staged = Path.Combine(data, "." + file + ".copy");
                Clone(obj, staged, size);
                Rename(staged, placed);
                File.SetAttributes(placed, FileAttributes.ReadOnly);
            }
        }
        foreach (string file in Directory.GetFiles(folder, "*", SearchOption.AllDirectories)) Remove(file);
        Directory.Delete(folder, true);
    }

    static void Work(object state)
    {
        int index = (int)state;
        Random random = new Random(index);
        Queue<string> kept = new Queue<string>();
        long size = 4096 * (1 + index % 4);
        byte[] bytes = new byte[size];
        random.NextBytes(bytes);
        long count = 0;
        try
        {
            while (!stop)
            {
                if (arm.StartsWith("tree-"))
                {
                    Tree(Path.Combine(root, "t" + index + "-" + count++), bytes, size, arm == "tree-link");
                    Interlocked.Increment(ref ops);
                    Interlocked.Exchange(ref last[index], Stopwatch.GetTimestamp());
                    continue;
                }
                string name = index + "-" + count++;
                string a = Path.Combine(root, "a" + random.Next(Folders));
                string b = Path.Combine(root, "b" + random.Next(Folders));
                string c = Path.Combine(root, "c" + random.Next(Folders));
                string written = Path.Combine(a, name + ".part");
                string named = Path.Combine(b, name + ".obj");
                bool sealing = arm.StartsWith("seal-");
                if (sealing) Seal(written, bytes); else File.WriteAllBytes(written, bytes);
                switch (arm)
                {
                    case "plain":
                        File.Delete(written);
                        break;
                    case "link":
                        Check(CreateHardLinkW(named, written, IntPtr.Zero), "link " + named);
                        File.Delete(written);
                        kept.Enqueue(named);
                        break;
                    case "rename":
                        Rename(written, named);
                        kept.Enqueue(named);
                        break;
                    case "clone":
                        Clone(written, named, size);
                        File.Delete(written);
                        kept.Enqueue(named);
                        break;
                    case "fix":
                        Rename(written, named);
                        string staged = Path.Combine(c, "." + name + ".copy");
                        string placed = Path.Combine(c, name + ".csv");
                        Clone(named, staged, size);
                        Rename(staged, placed);
                        kept.Enqueue(named);
                        kept.Enqueue(placed);
                        break;
                    case "seal-link":
                        Check(CreateHardLinkW(named, written, IntPtr.Zero), "link " + named);
                        Remove(written);
                        string linked = Path.Combine(c, name + ".csv");
                        Check(CreateHardLinkW(linked, named, IntPtr.Zero), "link " + linked);
                        kept.Enqueue(named);
                        kept.Enqueue(linked);
                        break;
                    case "seal-fix":
                        // tempfile's rename clears the attributes first, and fetchloom seals again
                        File.SetAttributes(written, FileAttributes.Normal);
                        Rename(written, named);
                        File.SetAttributes(named, FileAttributes.ReadOnly);
                        string copy = Path.Combine(c, "." + name + ".copy");
                        string cloned = Path.Combine(c, name + ".csv");
                        Clone(named, copy, size);
                        Rename(copy, cloned);
                        File.SetAttributes(cloned, FileAttributes.ReadOnly);
                        kept.Enqueue(named);
                        kept.Enqueue(cloned);
                        break;
                    default:
                        throw new ArgumentException("no arm " + arm);
                }
                while (kept.Count > Kept)
                {
                    if (sealing) Remove(kept.Dequeue()); else File.Delete(kept.Dequeue());
                }
                Interlocked.Increment(ref ops);
                Interlocked.Exchange(ref last[index], Stopwatch.GetTimestamp());
            }
        }
        catch (Exception error)
        {
            failure = "thread " + index + ": " + error.Message;
            stop = true;
        }
    }

    static int Main(string[] args)
    {
        arm = args[0];
        root = args[1];
        int threads = int.Parse(args[2]);
        int seconds = int.Parse(args[3]);
        string progress = args[4];
        foreach (string prefix in new[] { "a", "b", "c" })
            for (int folder = 0; folder < Folders; folder++)
                Directory.CreateDirectory(Path.Combine(root, prefix + folder));
        last = new long[threads];
        Thread[] workers = new Thread[threads];
        long started = Stopwatch.GetTimestamp();
        for (int index = 0; index < threads; index++)
        {
            last[index] = started;
            workers[index] = new Thread(Work);
            workers[index].IsBackground = true;
            workers[index].Start(index);
        }
        using (StreamWriter writer = new StreamWriter(new FileStream(progress, FileMode.Create, FileAccess.Write, FileShare.ReadWrite)))
        {
            writer.AutoFlush = true;
            while (true)
            {
                Thread.Sleep(500);
                long now = Stopwatch.GetTimestamp();
                double stalled = 0;
                for (int index = 0; index < threads; index++)
                    stalled = Math.Max(stalled, (now - Interlocked.Read(ref last[index])) / (double)Stopwatch.Frequency);
                double elapsed = (now - started) / (double)Stopwatch.Frequency;
                writer.WriteLine("{0:F1} {1} {2:F1}", elapsed, Interlocked.Read(ref ops), stalled);
                if (failure != null)
                {
                    writer.WriteLine("failed " + failure);
                    return 2;
                }
                if (elapsed >= seconds) break;
            }
            stop = true;
            foreach (Thread worker in workers) worker.Join(15000);
            writer.WriteLine("end {0}", Interlocked.Read(ref ops));
        }
        return 0;
    }
}
