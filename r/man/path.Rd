\name{path}
\alias{path}
\title{Where a dataset is placed}
\description{
returns the absolute path of a dataset, or of a file or folder inside one, as
\code{fetchloom path} does. a dataset not in place yet is synced alone first, which may
download its data.
}
\usage{
path(name, project = ".", yes = FALSE, offline = FALSE, locked = FALSE)
}
\arguments{
\item{name}{a dataset name such as \code{"iris"}, or a path inside a dataset such as
\code{"iris/Iris.csv"}.}
\item{project}{the project folder, which holds data.toml, or any folder inside it.}
\item{yes}{\code{TRUE} approves a large download.}
\item{offline}{\code{TRUE} prevents network requests.}
\item{locked}{\code{TRUE} never changes data.lock: when the sync would, it stops with kind
\code{lock.outdated}.}
}
\value{
the absolute path, as a string.
}
\details{
when fetchloom fails, the helpers stop with a condition of class \code{fetchloom_error}. its
\code{kind}, such as \code{usage} or \code{network.unreachable}, and \code{message} are those of
fetchloom's JSON error; \code{details} lists supporting lines, \code{help} is advice for a person
or \code{NULL}, \code{code} is the exit code and \code{result} the JSON result fetchloom gave with
the error, or \code{NULL}. \code{kind} is \code{NA} when fetchloom gave no JSON answer, and
\code{code} is \code{NA} when the helper stopped without running fetchloom. each warning fetchloom
gives is an R warning of class \code{fetchloom_warning}. a missing fetchloom program stops with a
plain error that says how to install it.
}
\seealso{
\code{\link{sync}}, \code{\link{download}}, and the exit codes at
\url{https://nikhil-yn-aix.github.io/fetchloom/user/reference/exit-codes/}.
}
\examples{
\dontrun{
iris <- read.csv(fetchloom::path("iris/iris.csv"))
}
}
