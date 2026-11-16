\name{sync}
\alias{sync}
\title{Place a project's data}
\description{
makes the project's data folder match data.lock and returns the project's absolute root, as
\code{fetchloom sync} does.
}
\usage{
sync(project = ".", names = character(), yes = FALSE, offline = FALSE,
     strict = FALSE, locked = FALSE, verify = FALSE)
}
\arguments{
\item{project}{the project folder, which holds data.toml, or any folder inside it.}
\item{names}{dataset names, such as \code{"iris"}, to sync alone; the others keep their pins and
files.}
\item{yes}{\code{TRUE} approves a download above 10 GiB, or the size the \code{ask_above} setting
gives, which otherwise stops with kind \code{unconfirmed}.}
\item{offline}{\code{TRUE} prevents network requests.}
\item{strict}{\code{TRUE} stops with kind \code{data.not_ready} when the data is not ready to use
afterwards, as \code{sync --strict} does.}
\item{locked}{\code{TRUE} stops with kind \code{lock.outdated} instead of updating a missing or
out-of-date data.lock.}
\item{verify}{\code{TRUE} checks the SHA-256 of every placed file and of each cached file it
places, as \code{sync --verify} does.}
}
\value{
the project's absolute root, as a string.
}
\details{
a file edited by hand stays, reported as a warning. errors are conditions of class
\code{fetchloom_error}, as \code{\link{path}} describes.
}
\examples{
\dontrun{
fetchloom::sync(locked = TRUE)
}
}
