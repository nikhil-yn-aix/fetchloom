# fetchloom

uv for research data. made by Nikhil Y N at KairosLab India Private Limited.

declare the datasets your work needs, from any repository, DOI or URL. `fl sync` puts exactly those bytes on any machine, stores each file once per machine and once per lab, runs your preprocessing once for everyone, and keeps finding your data after the original link dies.

```
fl add zenodo:3242074 --select "**/*.edf"
fl add hf:datasets/imdb
fl sync
fl run
fl cite > data.bib
```

```python
import fetchloom as fl
path = fl.path("eeg")
```

## status

early development. there is nothing to install yet.

## citing

if fetchloom helped your research, a citation is appreciated and never required. use the "cite this repository" button on GitHub, or `CITATION.cff`.

## license

Apache-2.0, free for any use. see `LICENSE` and `NOTICE`.
