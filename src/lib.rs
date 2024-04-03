pub mod package;

use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::Mutex};

use chrono::{Datelike, Timelike, Utc};
use comemo::Prehashed;
use fontdb::Database;

use package::prepare_package;
use typst::{
    diag::{FileError, FileResult},
    eval::Tracer,
    foundations::{self, Datetime},
    model::Document,
    syntax::{FileId, Source, VirtualPath},
    text::{Font, FontBook, FontInfo},
    Library, World,
};
use typst_syntax::{highlight, parse, LinkedNode};
pub struct TaggedString {
    tag: String,
    offset: u64,
    length: u64,
    errors: Option<String>,
}
pub fn walk_node(tagged_strings: &mut Vec<TaggedString>, root: &LinkedNode) {
    let mut h = false;
    if let Some(t) = highlight(root) {
        h = true;
        let errors = if root.erroneous() {
            Some(
                root.errors()
                    .iter()
                    .map(|e| format!("{}", e.message.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        } else {
            None
        };
        let string = TaggedString {
            tag: t.tm_scope().to_owned(),
            offset: root.offset() as u64,
            length: root.range().count() as u64,
            errors,
        };
        tagged_strings.push(string);
    }
    let t = root.text();
    if t.is_empty() {
        for child in root.children() {
            walk_node(tagged_strings, &child);
        }
    }
}
pub fn highlight_source(source: String) -> Vec<TaggedString> {
    let s = parse(&source);
    let mut strings = vec![];
    let root = LinkedNode::new(&s);
    walk_node(&mut strings, &root);
    return strings;
}
pub fn load_fonts() -> (FontBook, Vec<Font>) {
    let mut fonts = vec![];
    let mut db = Database::new();
    db.load_system_fonts();

    let mut book = FontBook::new();
    for font_face in db.faces() {
        let info = db
            .with_face_data(font_face.id, FontInfo::new)
            .expect("database must contain this font");

        let path = match &font_face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => path,
            // We never add binary sources to the database, so there
            // shouln't be any.
            fontdb::Source::Binary(_) => continue,
        };
        let font_data = std::fs::read(path).unwrap();
        if let Some(font) = Font::new(font_data.into(), font_face.index) {
            fonts.push(font);
            book.push(info.unwrap());
        } else {
            println!("{:?} not found", path);
        }
    }
    for data in typst_assets::fonts() {
        let buffer = typst::foundations::Bytes::from_static(data);
        for (i, font) in Font::iter(buffer).enumerate() {
            book.push(font.info().clone());
            fonts.push(font);
        }
    }
    (book, fonts)
}

pub enum Output {
    Pdf(Vec<u8>),
    Svg(Vec<u8>),
    Png(Vec<u8>),
}
pub fn create_world(root: String) -> TypstWorld {
    TypstWorld::new(root)
}
pub struct TypstWorld {
    root: String,
    library: Prehashed<Library>,
    mainId: FileId,
    book: Prehashed<FontBook>,
    sources: Mutex<HashMap<FileId, Slot>>,
    files: Mutex<HashMap<FileId, typst::foundations::Bytes>>,
    fonts: Vec<Font>,
}
struct Slot {
    fingerprint: u128,
    source: Source,
    accessed: bool,
}
impl TypstWorld {
    pub fn new(root: String) -> Self {
        let (book, fonts) = load_fonts();
        Self {
            root,
            library: Prehashed::new(Library::builder().build()),
            mainId: FileId::new(None, VirtualPath::new("main.typ")),
            book: Prehashed::new(book),
            sources: Mutex::new(HashMap::new()),
            files: Mutex::new(HashMap::new()),
            fonts,
        }
    }
    pub fn compile(&self) -> Result<Document, String> {
        let mut tracer = Tracer::new();
        let document = typst::compile(self, &mut tracer).unwrap();
        self.reset();
        Ok(document)
    }
    pub fn compile_pdf(&self) -> Result<Vec<u8>, CompilationError> {
        let mut tracer = Tracer::new();
        let document = match typst::compile(self, &mut tracer) {
            Ok(doc) => doc,
            Err(e) => {
                self.reset();
                return Err(CompilationError::CompilationError {
                    inner: format!("{e:?}"),
                });
            }
        };
        self.reset();
        Ok(typst_pdf::pdf(
            &document,
            typst::foundations::Smart::Auto,
            self.today(None),
        ))
    }
    fn reset(&self) {
        let Ok(mut files) = self.sources.lock() else {
            return;
        };
        for entry in files.iter_mut() {
            entry.1.accessed = false;
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum CompilationError {
    #[error("Failed to compile")]
    CompilationError { inner: String },
}

impl World for TypstWorld {
    #[doc = r" The standard library."]
    #[doc = r""]
    #[doc = r" Can be created through `Library::build()`."]
    fn library(&self) -> &Prehashed<Library> {
        &self.library
    }

    #[doc = r" Metadata about all known fonts."]
    fn book(&self) -> &Prehashed<FontBook> {
        &self.book
    }

    #[doc = r" Access the main source file."]
    fn main(&self) -> Source {
        self.source(self.mainId).unwrap().clone()
    }

    #[doc = r" Try to access the specified source file."]
    fn source(&self, id: FileId) -> FileResult<Source> {
        let Ok(mut file_lock) = self.sources.lock() else {
            return FileResult::Err(FileError::AccessDenied);
        };
        if let Some(file) = file_lock.get(&id) {
            if file.accessed {
                return Ok(file.source.clone());
            }
        }
        let path = id.vpath();
        let root;
        if let Some(spec) = id.package() {
            root = prepare_package(&self.root, spec)?;
        } else {
            root = PathBuf::from_str(&self.root).unwrap();
        }
        let Some(file_path) = path.resolve(&root) else {
            return FileResult::Err(FileError::AccessDenied);
        };
        let Ok(source_file) = std::fs::read_to_string(file_path.clone()) else {
            return FileResult::Err(FileError::NotFound(file_path));
        };
        let fingerprint = typst::util::hash128(&source_file);

        let t = file_lock.entry(id).or_insert_with(|| Slot {
            fingerprint,
            source: Source::new(id, source_file.clone()),
            accessed: true,
        });
        if fingerprint != t.fingerprint {
            t.fingerprint = fingerprint;
            t.source.replace(&source_file);
        }
        t.accessed = true;
        FileResult::Ok(t.source.clone())
    }

    #[doc = r" Try to access the specified file."]
    fn file(&self, id: FileId) -> Result<typst::foundations::Bytes, FileError> {
        let pathbuf;
        if let Some(spec) = id.package() {
            let buf = prepare_package(&self.root, spec)?;
            pathbuf = id.vpath().resolve(&buf).ok_or(FileError::AccessDenied)?;
        } else {
            pathbuf = id
                .vpath()
                .resolve(&PathBuf::from_str(&self.root).unwrap())
                .ok_or(FileError::AccessDenied)?;
        }
        let file = std::fs::read(&pathbuf).map_err(|_| FileError::NotFound(pathbuf))?;
        Ok(file.into())
    }

    #[doc = r" Try to access the font with the given index in the font book."]
    fn font(&self, index: usize) -> Option<Font> {
        let f = &self.fonts[index];
        Some(f.to_owned())
    }

    #[doc = r" Get the current date."]
    #[doc = r""]
    #[doc = r" If no offset is specified, the local date should be chosen. Otherwise,"]
    #[doc = r" the UTC date should be chosen with the corresponding offset in hours."]
    #[doc = r""]
    #[doc = r" If this function returns `None`, Typst's `datetime` function will"]
    #[doc = r" return an error."]
    fn today(&self, offset: Option<i64>) -> Option<Datetime> {
        let utc = Utc::now();
        Datetime::from_ymd_hms(
            utc.year(),
            utc.month() as u8,
            utc.day() as u8,
            utc.hour() as u8,
            utc.minute() as u8,
            utc.second() as u8,
        )
    }
}

uniffi::include_scaffolding!("mobiletypst");
