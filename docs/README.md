<div class="fl-home" markdown>

<div class="fl-intro" markdown>

# The same research data on every machine

<p class="fl-lede">fetchloom adds a dataset to your project and records each file's size and SHA&#8209;256 in data.lock. One shared cache keeps every file once, and anyone who clones the project gets exactly the same bytes.</p>

<p class="fl-step">Install it with Rust</p>

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
```

<p class="fl-step">Add your first dataset</p>

```sh
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
```

</div>

<figure class="fl-label" data-file="Iris.csv">
<figcaption>Like a specimen in a collection, every file is pinned with a label: where it came from, its size and its SHA&#8209;256.</figcaption>
</figure>

<div class="fl-where fl-mine" markdown>

<p class="fl-step">On your machine</p>

```text
$ fetchloom add https://huggingface.co/datasets/scikit-learn/iris
```

</div>

<div class="fl-where fl-theirs" markdown>

<p class="fl-step">On a teammate's machine, after <code>git clone</code></p>

```text
$ fetchloom sync
```

</div>

<nav class="fl-routes" aria-label="Guides" markdown>

- [Getting started](user/getting-started.md) Install it, try a first project and uninstall it
- [Concepts](user/concepts.md) What data.toml, data.lock and the cache each hold
- [Commands](user/commands/README.md) Every command, with its options and examples
- [Sources](user/sources/README.md) Zenodo, Hugging Face, Kaggle, S3 and the rest
- [Developer guide](dev/README.md) How fetchloom is built, tested and measured

</nav>

</div>
