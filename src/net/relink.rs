//! links a source gives only for a while, as semantic scholar's to its datasets' files: each
//! download asks the source for a fresh one by the stable link data.lock keeps, and once more when
//! the one it got answered that it expired.

use std::time::SystemTime;

use url::Url;

use super::{Net, failure};
use crate::error::Error;
use crate::source::Relink;

/// fresh links one download may ask for after the one before expired.
const MOST_RELINKS: u32 = 3;

impl Net {
    /// a signed link to the file `relink` names, fresh enough to start a download on. a link past
    /// its expiry, or the `failed` one that answered that it expired, is asked for again once,
    /// rather than taken from this run's memory of the listing.
    pub(super) async fn relinked(
        &self,
        relink: &Relink,
        failed: Option<&Url>,
    ) -> Result<Url, Error> {
        let link = relink.link(self).await?;
        let stale = failed == Some(&link) || crate::source::link_expired(&link, SystemTime::now());
        if !stale {
            return Ok(link);
        }
        tracing::debug!(link = %super::Shown(&link), "a signed link expired, so the source is asked for a fresh one");
        self.metadata.invalidate();
        let fresh = relink.link(self).await?;
        if failed == Some(&fresh) {
            return Err(relink.stale());
        }
        Ok(fresh)
    }

    /// a fresh link in place of `from`, which expired during a download, unless the source gave
    /// links that expired too often already. the download holds no slot while it asks.
    pub(super) async fn refreshed(
        &self,
        relink: &Relink,
        from: &Url,
        relinks: &mut u32,
    ) -> Result<Url, Error> {
        *relinks += 1;
        if *relinks > MOST_RELINKS {
            return Err(relink.stale());
        }
        let fresh = self.relinked(relink, Some(from)).await?;
        self.typed
            .check(&fresh)
            .map_err(|reason| failure::refused(&fresh, &reason))?;
        Ok(fresh)
    }
}
