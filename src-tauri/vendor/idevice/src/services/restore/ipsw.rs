//! IPSW archive access
//!
//! An IPSW is a ZIP archive containing a `BuildManifest.plist` and the firmware
//! components referenced by it. This module wraps [`async_zip`] to read the
//! manifest and extract components by their build-identity path.

use async_zip::base::read::seek::ZipFileReader;
use futures::AsyncReadExt as _;
use tokio::io::{AsyncBufRead, AsyncSeek, AsyncWriteExt as _};

use crate::{IdeviceError, services::restore::RestoreError};

/// A reader over an IPSW archive.
///
/// Generic over any seekable async source (a `tokio::fs::File`, an in-memory
/// cursor, etc.). The central directory is parsed once on construction.
pub struct Ipsw<R>
where
    R: AsyncBufRead + AsyncSeek + Unpin,
{
    zip: async_zip::tokio::read::seek::ZipFileReader<R>,
}

impl<R> std::fmt::Debug for Ipsw<R>
where
    R: AsyncBufRead + AsyncSeek + Unpin,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ipsw").finish_non_exhaustive()
    }
}

impl<R> Ipsw<R>
where
    R: AsyncBufRead + AsyncSeek + Unpin,
{
    /// Opens an IPSW from a seekable async reader.
    pub async fn new(reader: R) -> Result<Self, IdeviceError> {
        let zip = ZipFileReader::with_tokio(reader).await.map_err(|e| {
            IdeviceError::Restore(RestoreError::Ipsw(format!("failed to open IPSW: {e}")))
        })?;
        Ok(Self { zip })
    }

    /// Returns the archive entry index for an exact path, if present.
    fn entry_index(&self, path: &str) -> Option<usize> {
        self.zip
            .file()
            .entries()
            .iter()
            .position(|e| e.filename().as_str().map(|f| f == path).unwrap_or(false))
    }

    /// Reads a file from the archive by its exact path into memory.
    ///
    /// Suitable for firmware components (a few MB at most). The filesystem DMG
    /// should be streamed instead (see the ASR path).
    ///
    /// # Errors
    /// Returns [`IdeviceError::Ipsw`] if the entry is absent or cannot be read.
    pub async fn read_file(&mut self, path: &str) -> Result<Vec<u8>, IdeviceError> {
        let idx = self.entry_index(path).ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "entry `{path}` not found in IPSW"
            )))
        })?;
        let mut reader = self.zip.reader_with_entry(idx).await.map_err(|e| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "failed to open entry `{path}`: {e}"
            )))
        })?;
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.map_err(|e| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "failed to read entry `{path}`: {e}"
            )))
        })?;
        Ok(buf)
    }

    /// Streams an archive entry into a caller-supplied async writer (for large
    /// images like the filesystem DMG that can't be buffered in memory).
    ///
    /// idevice makes no host assumptions: the caller owns the sink. Native
    /// consumers can pass a `tokio::fs::File`; wasm/embedded consumers can pass
    /// whatever y'all use, etc. `writer` is flushed on success.
    ///
    /// # Errors
    /// Returns [`IdeviceError::Ipsw`] if the entry is absent or I/O fails.
    pub async fn extract_to_writer<W>(
        &mut self,
        path: &str,
        writer: &mut W,
    ) -> Result<(), IdeviceError>
    where
        W: tokio::io::AsyncWrite + Unpin,
    {
        let idx = self.entry_index(path).ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "entry `{path}` not found in IPSW"
            )))
        })?;
        let mut reader = self.zip.reader_with_entry(idx).await.map_err(|e| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "failed to open entry `{path}`: {e}"
            )))
        })?;

        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf).await.map_err(|e| {
                IdeviceError::Restore(RestoreError::Ipsw(format!("read `{path}`: {e}")))
            })?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n]).await.map_err(|e| {
                IdeviceError::Restore(RestoreError::Ipsw(format!("write `{path}`: {e}")))
            })?;
        }
        writer.flush().await.map_err(|e| {
            IdeviceError::Restore(RestoreError::Ipsw(format!("flush `{path}`: {e}")))
        })?;
        Ok(())
    }

    /// Opens a streaming [`ComponentReader`](super::state_machine::ComponentReader)
    /// over the archive entry `path`, reading directly from the archive without
    /// buffering the whole component in memory.
    pub(crate) async fn open_entry_reader<'a>(
        &'a mut self,
        path: &str,
    ) -> Result<Box<dyn super::state_machine::ComponentReader + Send + 'a>, IdeviceError>
    where
        R: Send,
    {
        let idx = self.entry_index(path).ok_or_else(|| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "entry `{path}` not found in IPSW"
            )))
        })?;
        let reader = self.zip.reader_with_entry(idx).await.map_err(|e| {
            IdeviceError::Restore(RestoreError::Ipsw(format!(
                "failed to open entry `{path}`: {e}"
            )))
        })?;
        Ok(Box::new(EntryReader { inner: reader }))
    }

    /// Reads and parses `BuildManifest.plist`.
    pub async fn build_manifest(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let bytes = self.read_file("BuildManifest.plist").await?;
        match plist::from_bytes::<plist::Value>(&bytes)? {
            plist::Value::Dictionary(d) => Ok(d),
            _ => Err(IdeviceError::Restore(RestoreError::Ipsw(
                "BuildManifest.plist is not a dictionary".into(),
            ))),
        }
    }

    /// Reads the component named `component` for the given build identity,
    /// resolving its path via the identity's `Manifest`.
    pub async fn read_component(
        &mut self,
        build_identity: &plist::Dictionary,
        component: &str,
    ) -> Result<Vec<u8>, IdeviceError> {
        let path = component_path(build_identity, component)?;
        self.read_file(&path).await
    }
}

/// Adapts an `async_zip` per-entry reader (a `futures` `AsyncRead`) to
/// [`ComponentReader`](super::state_machine::ComponentReader).
struct EntryReader<T> {
    inner: T,
}

impl<T> super::state_machine::ComponentReader for EntryReader<T>
where
    T: futures::AsyncRead + Unpin + Send,
{
    fn read<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<usize, IdeviceError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.inner.read(buf).await.map_err(|e| {
                IdeviceError::Restore(RestoreError::Ipsw(format!("read archive entry: {e}")))
            })
        })
    }
}

/// Resolves the archive path of a component from a build identity's `Manifest`.
///
/// Looks up `Manifest[component].Info.Path`.
///
/// # Errors
/// Returns [`IdeviceError::ComponentNotFound`] if the component or its path is
/// missing.
pub fn component_path(
    build_identity: &plist::Dictionary,
    component: &str,
) -> Result<String, IdeviceError> {
    build_identity
        .get("Manifest")
        .and_then(|m| m.as_dictionary())
        .and_then(|m| m.get(component))
        .and_then(|c| c.as_dictionary())
        .and_then(|c| c.get("Info"))
        .and_then(|i| i.as_dictionary())
        .and_then(|i| i.get("Path"))
        .and_then(|p| p.as_string())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            IdeviceError::Restore(RestoreError::ComponentNotFound(component.to_string()))
        })
}

/// Names of the components iBoot loads during the restore boot, in manifest
/// order: those whose `Info` has `IsLoadedByiBoot` set but not
/// `IsLoadedByiBootStage1`. Each is uploaded and followed by a `firmware` command.
pub fn components_loaded_by_iboot(build_identity: &plist::Dictionary) -> Vec<String> {
    let mut out = Vec::new();
    let Some(manifest) = build_identity
        .get("Manifest")
        .and_then(|m| m.as_dictionary())
    else {
        return out;
    };
    for (name, node) in manifest {
        let Some(info) = node
            .as_dictionary()
            .and_then(|n| n.get("Info"))
            .and_then(|i| i.as_dictionary())
        else {
            continue;
        };
        let iboot = info
            .get("IsLoadedByiBoot")
            .and_then(|v| v.as_boolean())
            .unwrap_or(false);
        let stage1 = info
            .get("IsLoadedByiBootStage1")
            .and_then(|v| v.as_boolean())
            .unwrap_or(false);
        if iboot && !stage1 {
            out.push(name.clone());
        }
    }
    out
}
