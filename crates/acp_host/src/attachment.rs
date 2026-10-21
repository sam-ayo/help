//! Files that the user attaches to a message, and files that the agent sends back.
//!
//! Each file goes to the agent in the best form that the agent accepts: an image block for an
//! image, an embedded resource for a text file, and a resource link for other files. Every agent
//! accepts a resource link, so a file that the agent cannot take in a different form goes as a
//! link to its path.

use std::{
    hash::{DefaultHasher, Hash as _, Hasher as _},
    path::{Path, PathBuf},
};

use agent_client_protocol::schema::v1::*;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// The largest image that goes to the agent as an image block. The Claude API takes images of
/// up to 5 MB in base64, which is about 3.75 MB of data. A larger image goes as a link.
const MAX_IMAGE: u64 = 3_750_000;
/// The largest text file that goes to the agent in the prompt. A larger file goes as a link.
const MAX_TEXT: u64 = 256 * 1024;
/// The image types that the agents can see.
const AGENT_IMAGES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// A file on disk, in a message of the user or the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl Attachment {
    /// The attachment for the file at `path`.
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        let meta = std::fs::metadata(path)?;
        if meta.is_dir() {
            return Err(std::io::Error::other(format!("{} is a folder", path.display())));
        }
        Ok(Self {
            path: path.to_path_buf(),
            name: path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into()),
            mime: mime_for(path).into(),
            size: meta.len(),
        })
    }

    /// Keep `bytes` in a file in `dir`, and return the attachment for it. The file name comes
    /// from the content, so the same content gets the same file.
    pub fn save(dir: &Path, name: Option<&str>, mime: &str, bytes: &[u8]) -> std::io::Result<Self> {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = format!("{:016x}", hasher.finish());
        let file = match name.map(sanitize).filter(|n| !n.is_empty()) {
            Some(name) => format!("{hash}-{name}"),
            None => format!("{hash}.{}", extension_for(mime)),
        };
        std::fs::create_dir_all(dir)?;
        let path = dir.join(file);
        if !path.exists() {
            std::fs::write(&path, bytes)?;
        }
        let mut attachment = Self::from_path(&path)?;
        attachment.mime = mime.into();
        attachment.name = match name {
            Some(name) => name.into(),
            None if mime.starts_with("image/") => format!("Image.{}", extension_for(mime)),
            None => format!("File.{}", extension_for(mime)),
        };
        Ok(attachment)
    }

    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }

    pub fn is_video(&self) -> bool {
        self.mime.starts_with("video/")
    }

    fn uri(&self) -> String {
        file_uri(&self.path)
    }
}

/// A file name without path separators or control characters.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c == '/' || c == ':' || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

/// A `file://` URI for `path`, with the characters that a URI cannot have in percent form.
pub fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => uri.push(b as char),
            _ => uri.push_str(&format!("%{b:02X}")),
        }
    }
    uri
}

/// The path of a `file://` URI, or `None` for a different URI.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // A host can come before the path, for example `file://localhost/tmp`.
    let rest = &rest[rest.find('/')?..];
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

/// The MIME type of a file, from its extension.
pub fn mime_for(path: &Path) -> &'static str {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "mov" => "video/quicktime",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "aiff" | "aif" => "audio/aiff",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "dmg" => "application/x-apple-diskimage",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pages" => "application/vnd.apple.pages",
        "numbers" => "application/vnd.apple.numbers",
        "key" => "application/vnd.apple.keynote",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "xml" => "text/xml",
        "txt" | "log" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/tiff" => "tiff",
        "image/bmp" => "bmp",
        "application/pdf" => "pdf",
        "application/json" => "json",
        "text/markdown" => "md",
        "text/csv" => "csv",
        m if m.starts_with("text/") => "txt",
        _ => "bin",
    }
}

/// The text of a file, if the file is small text: valid UTF-8 with no NUL bytes.
fn read_text(a: &Attachment) -> Option<String> {
    if a.size > MAX_TEXT || a.is_image() || a.is_video() || a.mime.starts_with("audio/") {
        return None;
    }
    let bytes = std::fs::read(&a.path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// The content blocks of a prompt: the text, then one block for each attachment.
pub(crate) fn prompt_blocks(text: String, attachments: &[Attachment], caps: &PromptCapabilities) -> Vec<ContentBlock> {
    let mut blocks = Vec::with_capacity(attachments.len() + 1);
    if !text.is_empty() {
        blocks.push(text.into());
    }
    for a in attachments {
        blocks.push(prompt_block(a, caps));
    }
    blocks
}

fn prompt_block(a: &Attachment, caps: &PromptCapabilities) -> ContentBlock {
    if caps.image && AGENT_IMAGES.contains(&a.mime.as_str()) && a.size <= MAX_IMAGE {
        match std::fs::read(&a.path) {
            Ok(bytes) => {
                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                return ContentBlock::Image(ImageContent::new(data, a.mime.clone()).uri(a.uri()));
            }
            Err(e) => tracing::warn!("cannot read {}: {e}", a.path.display()),
        }
    }
    if caps.embedded_context
        && let Some(text) = read_text(a)
    {
        let mime = (a.mime != "application/octet-stream").then(|| a.mime.clone());
        let contents = TextResourceContents::new(text, a.uri()).mime_type(mime);
        return ContentBlock::Resource(EmbeddedResource::new(EmbeddedResourceResource::TextResourceContents(
            contents,
        )));
    }
    ContentBlock::ResourceLink(
        ResourceLink::new(a.name.clone(), a.uri())
            .mime_type(a.mime.clone())
            .size(i64::try_from(a.size).ok()),
    )
}

/// What a content block from the agent becomes in the conversation.
#[derive(Debug, PartialEq)]
pub(crate) enum Received {
    Text(String),
    File(Attachment),
}

/// Turn a content block from the agent into text or a file. Image and binary data go to a file
/// in `dir`. A link to a file on this Mac becomes that file. A different link becomes a
/// Markdown link.
pub(crate) fn receive(block: ContentBlock, dir: &Path) -> Option<Received> {
    let decode = |data: &str| base64::engine::general_purpose::STANDARD.decode(data.trim()).ok();
    let save = |name: Option<&str>, mime: &str, bytes: Vec<u8>| match Attachment::save(dir, name, mime, &bytes) {
        Ok(a) => Some(Received::File(a)),
        Err(e) => {
            tracing::warn!("cannot keep a file from the agent: {e}");
            None
        }
    };
    let name_of = |uri: &str| uri.rsplit('/').next().filter(|n| !n.is_empty()).map(String::from);
    match block {
        ContentBlock::Text(t) => Some(Received::Text(t.text)),
        ContentBlock::Image(i) => {
            let name = i.uri.as_deref().and_then(name_of);
            save(name.as_deref(), &i.mime_type, decode(&i.data)?)
        }
        ContentBlock::Audio(a) => save(None, &a.mime_type, decode(&a.data)?),
        ContentBlock::ResourceLink(link) => match uri_path(&link.uri) {
            Some(path) if path.is_file() => Attachment::from_path(&path).ok().map(Received::File),
            _ => Some(Received::Text(format!("[{}]({})", link.name, link.uri))),
        },
        ContentBlock::Resource(r) => match r.resource {
            EmbeddedResourceResource::TextResourceContents(t) => {
                let mime = t.mime_type.unwrap_or_else(|| "text/plain".into());
                save(name_of(&t.uri).as_deref(), &mime, t.text.into_bytes())
            }
            EmbeddedResourceResource::BlobResourceContents(b) => {
                let mime = b.mime_type.unwrap_or_else(|| "application/octet-stream".into());
                save(name_of(&b.uri).as_deref(), &mime, decode(&b.blob)?)
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("help-attach-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn caps(image: bool, embedded: bool) -> PromptCapabilities {
        let mut caps = PromptCapabilities::default();
        caps.image = image;
        caps.embedded_context = embedded;
        caps
    }

    #[test]
    fn a_uri_and_its_path_go_both_ways() {
        let path = Path::new("/Users/me/My Files/ä#1.png");
        let uri = file_uri(path);
        assert_eq!(uri, "file:///Users/me/My%20Files/%C3%A4%231.png");
        assert_eq!(uri_path(&uri).as_deref(), Some(path));
        assert_eq!(uri_path("file://localhost/tmp/a").as_deref(), Some(Path::new("/tmp/a")));
        assert_eq!(uri_path("https://example.com/a"), None);
    }

    #[test]
    fn each_file_goes_in_the_best_form_that_the_agent_accepts() {
        let dir = temp_dir("blocks");
        let png = Attachment::save(&dir, None, "image/png", b"\x89PNG fake").unwrap();
        let text = Attachment::save(&dir, Some("notes.md"), "text/markdown", b"# Notes").unwrap();
        let pdf = Attachment::save(&dir, Some("a.pdf"), "application/pdf", b"%PDF\0").unwrap();
        let heic = Attachment::save(&dir, Some("b.heic"), "image/heic", b"heic").unwrap();
        let files = [png.clone(), text.clone(), pdf, heic];

        let blocks = prompt_blocks("look".into(), &files, &caps(true, true));
        assert!(matches!(&blocks[0], ContentBlock::Text(t) if t.text == "look"));
        assert!(matches!(&blocks[1], ContentBlock::Image(i) if i.mime_type == "image/png"));
        assert!(matches!(&blocks[2], ContentBlock::Resource(_)));
        assert!(matches!(&blocks[3], ContentBlock::ResourceLink(l) if l.name == "a.pdf"));
        // The agents cannot see HEIC images, so the image goes as a link.
        assert!(matches!(&blocks[4], ContentBlock::ResourceLink(l) if l.name == "b.heic"));

        // An agent with no image and no embedded context gets links.
        let blocks = prompt_blocks(String::new(), &[png, text], &caps(false, false));
        assert_eq!(blocks.len(), 2);
        assert!(blocks.iter().all(|b| matches!(b, ContentBlock::ResourceLink(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_from_the_agent_go_to_disk() {
        let dir = temp_dir("receive");
        let data = base64::engine::general_purpose::STANDARD.encode(b"img");
        let Some(Received::File(a)) = receive(ContentBlock::Image(ImageContent::new(data.clone(), "image/png")), &dir)
        else {
            panic!("no file");
        };
        assert_eq!(std::fs::read(&a.path).unwrap(), b"img");
        assert!(a.is_image() && a.path.extension().is_some_and(|e| e == "png"));
        // The same content gets the same file.
        let again = receive(ContentBlock::Image(ImageContent::new(data, "image/png")), &dir);
        assert_eq!(again, Some(Received::File(a.clone())));

        let link = ContentBlock::ResourceLink(ResourceLink::new("shot", file_uri(&a.path)));
        assert_eq!(receive(link, &dir), Some(Received::File(Attachment { name: a.path.file_name().unwrap().to_string_lossy().into(), ..a })));
        let web = ContentBlock::ResourceLink(ResourceLink::new("site", "https://example.com"));
        assert_eq!(receive(web, &dir), Some(Received::Text("[site](https://example.com)".into())));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
