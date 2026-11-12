# GEO

GEO references list the files NCBI's gene expression omnibus keeps for a series, a sample or a platform, from the indexes of NCBI's file server over https, where GEOquery finds them too. public records need no account.

```sh
fetchloom add geo:GSE1000 --select "matrix/*"
fetchloom add https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GSE1000 --as gse1000 --select "suppl/*"
fetchloom get geo:GSM2666939
```

## references

| spelling | behavior |
|---|---|
| `geo:GSE1000` | a series' files |
| `geo:GSM2666939` | a sample's supplementary files |
| `geo:GPL96` | a platform's files |
| `https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GSE1000` | accept an accession's page on GEO's site |
| `https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/` | accept an accession's folder on NCBI's file server; a link to a folder inside it, as `…/GSE1000/suppl/`, selects that folder |

accessions are written in capitals, whatever case they are typed in. a link to one file on NCBI's file server stays an ordinary [URL](urls-and-dois.md), and so do NCBI's other pages. GEO DataSets (GDS) are not listed, nor the SOFT text GEO's site makes for an accession on request, as GEOquery downloads a sample's.

## which files

each file keeps its path below the accession's folder, by the folder GEO keeps it in:

| folder | what GEO keeps there |
|---|---|
| `matrix/` | a series' matrix tables, one for each platform it used |
| `soft/`, `miniml/` | a series' or platform's records as SOFT text or MINiML XML |
| `suppl/` | the submitter's supplementary files: a series often has one tar of every sample's files, with `filelist.txt` naming them |
| `annot/` | a platform's annotation table |

select by folder, as `--select "matrix/*"`; some files are large, such as GPL96's 6.6 GB `soft/GPL96_family.soft.gz`. a sample without supplementary files has no folder, and neither has an accession that does not exist or is not public yet: the add stops with `reference.unresolved`, naming the accession's page.

## pins

`resolved` is the reference itself, as `geo:GSE1000`; each file's `url` is its link on ftp.ncbi.nlm.nih.gov. NCBI states no checksums for these files, so the SHA-256 the lock records is all that names their bytes. its indexes round sizes, as `1.3M`, so a file's size comes from its download rather than from the index.

GEO remakes a series' SOFT, MINiML and matrix files from time to time, as the dates in their folders show, so a file pinned once can stop matching. a sync that receives other bytes stops with `integrity.mismatch`, exit 30, and places nothing; [push](../commands/push.md) the pinned bytes to a mirror to keep them, or run [`lock --upgrade`](../commands/lock.md#upgrading) to take the new ones. `access` is `public`.

## limits

NCBI asks a program without a key to send no more than three requests a second. fetchloom starts requests to NCBI's hosts at least a third of a second apart, downloads included, so listing a series, its folder and the four inside it, takes about a second and a half. GEO is not in [search](../commands/search.md), and records GEO keeps private for reviewers are not listed.
