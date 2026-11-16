\name{download}
\alias{download}
\title{Download a link once}
\description{
downloads a link once, without a project, and returns where it went, as \code{fetchloom get}
does. it is not called \code{get}, which would hide \code{base::get} once the package is
attached.
}
\usage{
download(link, path = NULL, folder = NULL, sha256 = NULL, force = FALSE,
         yes = FALSE, offline = FALSE)
}
\arguments{
\item{link}{a link, a DOI or an id such as \code{"zenodo:3608990"}.}
\item{path}{where to write the one file the link resolves to. a file already there that holds
the same bytes stays as it is; one holding other bytes stops with kind \code{local.not_empty}
unless \code{force} is \code{TRUE}.}
\item{folder}{the folder to fill, in place of a new folder named after the download. give
\code{path} or \code{folder}, not both.}
\item{sha256}{the hash the one file must have.}
\item{force}{\code{TRUE} replaces the file at \code{path}.}
\item{yes}{\code{TRUE} approves a large download.}
\item{offline}{\code{TRUE} prevents network requests.}
}
\value{
\code{path} when given, else the folder the download went into, as a string.
}
\details{
errors are conditions of class \code{fetchloom_error}, as \code{\link{path}} describes.
}
\examples{
\dontrun{
table <- read.csv(fetchloom::download("https://example.org/data.csv", "data.csv"))
}
}
