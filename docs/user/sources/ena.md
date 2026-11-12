# ENA

ENA references list the read files of runs, as the european nucleotide archive's portal API reports them, and check each against the MD5 and size ENA states for it. ENA mirrors the INSDC archives, so runs submitted to NCBI's SRA or DDBJ's DRA are there too, under the same accessions. public runs need no account.

```sh
fetchloom add ena:ERR1160846
fetchloom add ena:SRX079566 --select "SRR390728/*"
fetchloom get https://www.ebi.ac.uk/ena/browser/view/SRX079566 --select "**/*_1.fastq.gz"
```

## references

| spelling | behavior |
|---|---|
| `ena:SRR390728` | a run's FASTQ files; runs are `SRR`, `ERR` or `DRR` |
| `ena:SRX079566` | the runs of an experiment: `SRX`, `ERX` or `DRX` |
| `ena:SAMN00630374` | the runs of a sample: `SAMN`, `SAMEA`, `SAMD`, or `SRS`, `ERS`, `DRS` |
| `ena:PRJNA172563` | the runs of a study: `PRJNA`, `PRJEB`, `PRJDB`, or `SRP`, `ERP`, `DRP` |
| `ena:submitted/ERR1160846` | the files as their submitter sent them, which may be FASTQ, BAM or CRAM |
| `ena:sra/SRR390728` | the run as one SRA file, which ENA keeps for few runs |
| `https://www.ebi.ac.uk/ena/browser/view/SRR390728` | accept an accession's page in ENA's browser |

accessions are written in capitals, whatever case they are typed in. other ebi.ac.uk links, such as a file on ftp.sra.ebi.ac.uk or a FASTA record from ENA's browser API, are ordinary [URLs](urls-and-dois.md).

## which files

a run accession names reads, not bytes. ENA keeps up to three kinds of file for a run, and they are different files: the FASTQ ENA makes from the reads, the files the submitter sent, and an SRA file. a reference lists one kind, so a lock never mixes them: FASTQ unless it names `submitted/` or `sra/`. FASTQ is the default because most runs have it, ENA states an MD5 for it, and tools read it directly. in practice few runs have an SRA file.

a run's files sit at the top of the dataset, as `ERR1160846.fastq.gz`. an experiment, a sample or a study can hold many runs, so each run's files sit in a folder named by its run, as `SRR390728/SRR390728_1.fastq.gz`. `--select "SRR390728/*"` takes one run, and `--select "**/*_1.fastq.gz"` the first reads of every run.

a run without files of the kind asked for is left out with a warning, which names the reference to the kind it has, as `ena:submitted/PRJEB102639`. when no run has any, the add stops with `reference.unresolved` and names it the same way. a run whose files are under controlled access, as dbGaP's are, is left out with a warning; when every run is, the add stops with `auth.required`, exit 40, naming the archive ENA sends people to. fetchloom downloads public files only.

## pins

`resolved` is the reference itself, as `ena:PRJNA172563`: an accession has no versions, and a study can gain runs. [`lock --upgrade`](../commands/lock.md#upgrading) lists it again, with any new runs. each file's `url` is its https link on ENA's file server, as `https://ftp.sra.ebi.ac.uk/vol1/fastq/ERR116/006/ERR1160846/ERR1160846.fastq.gz`; fetchloom downloads over https and needs no ftp, aspera or globus. downloads check the MD5 and size ENA states, then the lock records each file's SHA-256 and size; bytes that differ stop with `integrity.mismatch` and are never placed. `access` is `public`.

## limits

ENA sends a report whole: its portal API has no pages and refuses an `offset`. so fetchloom first asks how many runs an accession has, then asks for exactly that many, and refuses a report that holds fewer. a study with more runs than [`most_files`](../reference/environment.md) allows is refused before its report is downloaded; add the runs you need by their own accessions. listing a study of 1,153 runs takes two requests.

ENA's file server can take half a minute to start sending a file, small ones too, as ENA's own documentation warns of downloads under load. fetchloom waits for the answer as long as `FETCHLOOM_TIMEOUT` allows after connecting, then tries again.

ENA is not in [search](../commands/search.md).
