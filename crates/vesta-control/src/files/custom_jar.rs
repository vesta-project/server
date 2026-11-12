//! Bounded manifest observation for confirmed custom launch selection.
use super::*;
impl FileBrowser {
    pub(crate) fn custom_jar_manifest(
        &self,
        subject: &Subject,
        path: &RelativePath,
    ) -> Result<String> {
        let _stream = self.open_stream(subject, path)?;
        let checked = self.root.resolve(Some(path))?;
        let file = native::open_regular(&checked.path)?;
        let mut zip = zip::ZipArchive::new(file).map_err(|_| FileError::Corrupt)?;
        if zip.len() > 4096 {
            return Err(FileError::TooLarge);
        }
        let file = zip
            .by_name("META-INF/MANIFEST.MF")
            .map_err(|_| FileError::Corrupt)?;
        if file.size() > 65536 {
            return Err(FileError::TooLarge);
        }
        let mut text = String::new();
        file.take(65537)
            .read_to_string(&mut text)
            .map_err(io_error)?;
        if text.len() > 65536 {
            return Err(FileError::TooLarge);
        }
        let text = text.replace("\r\n ", "").replace("\n ", "");
        let main = text
            .lines()
            .take_while(|line| !line.is_empty())
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("Main-Class"))
                    .map(|(_, v)| v.trim())
            })
            .ok_or(FileError::Corrupt)?;
        if main.is_empty()
            || main.len() > 512
            || !main
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'$'))
        {
            return Err(FileError::Corrupt);
        }
        Ok(main.into())
    }
}
