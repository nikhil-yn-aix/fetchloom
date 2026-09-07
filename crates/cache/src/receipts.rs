//! Where a run's receipt is kept, and how it is found again.

use std::path::Path;

use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::receipt::Receipt;
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;

impl<P: Platform> Cache<P> {
    /// # Errors
    /// `manifest.invalid` when the receipt cannot be rendered, `cache.corrupt`
    /// when it cannot be written or renamed into place, and `resource.disk`
    /// when the volume is full.
    pub fn write_receipt(&self, receipt: &Receipt) -> Result<(), Error> {
        let path = self.layout().receipt_of(Receipt::key(&receipt.destination));
        let rendered = receipt.render()?;
        let mut beside = path.as_os_str().to_owned();
        beside.push(format!(".{}.writing", std::process::id()));
        let beside = std::path::PathBuf::from(beside);
        std::fs::write(&beside, rendered.as_bytes())
            .map_err(|reason| filesystem_failure(Surface::Cache, &beside, &reason))?;
        self.work().touched_file();
        std::fs::rename(&beside, &path).map_err(|reason| {
            let _ = std::fs::remove_file(&beside);
            filesystem_failure(Surface::Cache, &path, &reason)
        })?;
        self.work().touched_file();
        Ok(())
    }

    /// # Errors
    /// `cache.corrupt` when the receipt exists and cannot be removed. No
    /// receipt for that destination is success rather than an error.
    pub fn forget_receipt(&self, destination: &Path) -> Result<(), Error> {
        let path = self.layout().receipt_of(Receipt::key(destination));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(reason) => Err(filesystem_failure(Surface::Cache, &path, &reason)),
        }
    }

    /// # Errors
    /// `cache.corrupt` when a receipt exists and cannot be read, and
    /// `manifest.invalid` when it does not parse. No receipt, or one written
    /// for another destination, is `None` rather than an error.
    pub fn read_receipt(&self, destination: &Path) -> Result<Option<Receipt>, Error> {
        let path = self.layout().receipt_of(Receipt::key(destination));
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(reason) if reason.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(reason) => return Err(filesystem_failure(Surface::Cache, &path, &reason)),
        };
        let receipt = Receipt::parse(&bytes, &Limits::default())?;
        if receipt.destination == destination {
            Ok(Some(receipt))
        } else {
            Ok(None)
        }
    }
}
