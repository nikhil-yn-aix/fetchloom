//! Where a run's receipt is kept, and how it is found again.

use std::path::Path;

use fetchloom_engine::error::{Error, Surface, filesystem_failure};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::receipt::Receipt;
use fetchloom_engine::seam::platform::Platform;

use crate::Cache;

impl<P: Platform> Cache<P> {
    /// Writes the receipt for the destination it names.
    ///
    /// Writes beside the receipt and renames onto it.
    ///
    /// # Errors
    ///
    /// Fails when the receipt cannot be written.
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

    /// Returns the receipt that describes a destination.
    ///
    /// Takes the destination as it was resolved against the working directory.
    /// Returns nothing when no receipt is stored under that name and when the
    /// receipt stored there describes another destination.
    ///
    /// # Errors
    ///
    /// Fails when a receipt is present and cannot be read or does not parse.
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
