//! Word recognition with the system's libtesseract, loaded at run time through its C API, so
//! the build needs no headers and a machine without Tesseract still runs (and says why it
//! can't read the screen).

#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_float, c_int, c_uchar, c_void};

/// One recognised word, in the pixels of the image given to [`Ocr::words`].
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    pub conf: f32,
    /// left, top, right, bottom.
    pub bbox: [i32; 4],
    /// Which text line, in reading order.
    pub line: u32,
}

const RIL_TEXTLINE: c_int = 2;
const RIL_WORD: c_int = 3;
const PSM_SPARSE_TEXT: c_int = 11;

type Api = *mut c_void;
type Iter = *mut c_void;

struct Fns {
    create: unsafe extern "C" fn() -> Api,
    delete: unsafe extern "C" fn(Api),
    init3: unsafe extern "C" fn(Api, *const c_char, *const c_char) -> c_int,
    set_psm: unsafe extern "C" fn(Api, c_int),
    set_variable: unsafe extern "C" fn(Api, *const c_char, *const c_char) -> c_int,
    set_image: unsafe extern "C" fn(Api, *const c_uchar, c_int, c_int, c_int, c_int),
    set_resolution: unsafe extern "C" fn(Api, c_int),
    recognize: unsafe extern "C" fn(Api, *mut c_void) -> c_int,
    get_iterator: unsafe extern "C" fn(Api) -> Iter,
    clear: unsafe extern "C" fn(Api),
    iter_delete: unsafe extern "C" fn(Iter),
    iter_next: unsafe extern "C" fn(Iter, c_int) -> c_int,
    iter_text: unsafe extern "C" fn(Iter, c_int) -> *mut c_char,
    iter_conf: unsafe extern "C" fn(Iter, c_int) -> c_float,
    iter_page: unsafe extern "C" fn(Iter) -> *mut c_void,
    page_bbox: unsafe extern "C" fn(*mut c_void, c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_int) -> c_int,
    page_begins: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    delete_text: unsafe extern "C" fn(*const c_char),
    /// The library the functions are in, kept loaded with them.
    _lib: libloading::Library,
}

/// libtesseract, loaded once a process: recognisers come and go (`engine::Reader`), the library
/// (C++ with static state of its own) stays.
fn library() -> Result<&'static Fns, String> {
    static LOADED: std::sync::OnceLock<Fns> = std::sync::OnceLock::new();
    if let Some(f) = LOADED.get() { return Ok(f); }
    let lib = find_library()?;
    // SAFETY: each symbol is looked up by its C API name with the signature from capi.h.
    let f = unsafe {
        macro_rules! sym { ($n:literal) => { *lib.get($n).map_err(|e| format!("libtesseract lacks {}: {e}", String::from_utf8_lossy($n)))? } }
        Fns {
            create: sym!(b"TessBaseAPICreate"),
            delete: sym!(b"TessBaseAPIDelete"),
            init3: sym!(b"TessBaseAPIInit3"),
            set_psm: sym!(b"TessBaseAPISetPageSegMode"),
            set_variable: sym!(b"TessBaseAPISetVariable"),
            set_image: sym!(b"TessBaseAPISetImage"),
            set_resolution: sym!(b"TessBaseAPISetSourceResolution"),
            recognize: sym!(b"TessBaseAPIRecognize"),
            get_iterator: sym!(b"TessBaseAPIGetIterator"),
            clear: sym!(b"TessBaseAPIClear"),
            iter_delete: sym!(b"TessResultIteratorDelete"),
            iter_next: sym!(b"TessResultIteratorNext"),
            iter_text: sym!(b"TessResultIteratorGetUTF8Text"),
            iter_conf: sym!(b"TessResultIteratorConfidence"),
            iter_page: sym!(b"TessResultIteratorGetPageIterator"),
            page_bbox: sym!(b"TessPageIteratorBoundingBox"),
            page_begins: sym!(b"TessPageIteratorIsAtBeginningOf"),
            delete_text: sym!(b"TessDeleteText"),
            _lib: lib,
        }
    };
    Ok(LOADED.get_or_init(|| f))
}

/// A loaded, initialised Tesseract. Not `Sync`: one recogniser per thread.
pub struct Ocr {
    f: &'static Fns,
    api: Api,
}

// SAFETY: the handle is only used from the thread that owns the `Ocr`; moving it between
// threads is fine (Tesseract keeps no thread-local state in a TessBaseAPI).
unsafe impl Send for Ocr {}

const LIBRARIES: &[&str] = &["libtesseract.so.5", "libtesseract.so.5.5", "libtesseract.so.5.4", "libtesseract.so.5.3", "libtesseract.so"];
const TESSDATA: &[&str] = &[
    "/usr/share/tesseract/tessdata",
    "/usr/share/tesseract-ocr/5/tessdata",
    "/usr/share/tesseract-ocr/4.00/tessdata",
    "/usr/share/tessdata",
    "/usr/local/share/tessdata",
];

fn find_library() -> Result<libloading::Library, String> {
    let mut names: Vec<String> = LIBRARIES.iter().map(|s| s.to_string()).collect();
    // Any versioned soname in the usual library folders (a distro may ship 5.6 before we list it).
    for dir in ["/usr/lib64", "/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu"] {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.starts_with("libtesseract.so.") { names.push(format!("{dir}/{n}")); }
            }
        }
    }
    for name in &names {
        // SAFETY: loading libtesseract runs its static constructors, which are safe.
        if let Ok(lib) = unsafe { libloading::Library::new(name) } { return Ok(lib); }
    }
    Err("libtesseract isn't installed (Fedora: tesseract-libs tesseract-langpack-eng; Debian/Ubuntu: libtesseract5 tesseract-ocr-eng; Arch: tesseract tesseract-data-eng)".into())
}

/// The languages of a setting like `eng+ukr`.
fn languages(lang: &str) -> impl Iterator<Item = &str> { lang.split('+').map(str::trim).filter(|l| !l.is_empty()) }

/// The folder holding the data for `lang`: every language of one like `eng+ukr`.
pub fn tessdata_dir(lang: &str) -> Option<String> {
    languages(lang).next()?;
    let has = |dir: &std::path::Path| languages(lang).all(|l| dir.join(format!("{l}.traineddata")).exists());
    if let Some(prefix) = std::env::var_os("TESSDATA_PREFIX") {
        let p = std::path::PathBuf::from(prefix);
        for dir in [p.clone(), p.join("tessdata")] {
            if has(&dir) { return Some(dir.to_string_lossy().into_owned()); }
        }
    }
    TESSDATA.iter().find(|d| has(std::path::Path::new(d))).map(|d| d.to_string())
}

/// Why `lang` can't be read, when its data isn't installed: which of its languages are missing.
pub fn missing_data(lang: &str) -> Option<String> {
    if tessdata_dir(lang).is_some() { return None; }
    let installed = installed_languages();
    let missing: Vec<&str> = languages(lang).filter(|l| !installed.iter().any(|i| i == l)).collect();
    Some(match missing.len() {
        0 if languages(lang).next().is_none() => "no text recognition language is chosen".to_string(),
        0 => format!("the Tesseract data for {} isn't in one folder", lang.replace('+', " and ")),
        _ => format!("no Tesseract data for {} (install your distro's Tesseract language pack)", missing.join(" or ")),
    })
}

/// The Tesseract languages installed (`eng`, `ukr`, …), less the script detector.
pub fn installed_languages() -> Vec<String> {
    let mut dirs: Vec<std::path::PathBuf> = TESSDATA.iter().map(std::path::PathBuf::from).collect();
    if let Some(p) = std::env::var_os("TESSDATA_PREFIX").map(std::path::PathBuf::from) { dirs.insert(0, p.join("tessdata")); dirs.insert(0, p); }
    let mut v: Vec<String> = dirs.iter().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".traineddata").map(str::to_string))
        .filter(|l| l != "osd" && l != "equ").collect();
    v.sort();
    v.dedup();
    v
}

impl Ocr {
    pub fn new(lang: &str) -> Result<Ocr, String> {
        let f = library()?;
        let data = tessdata_dir(lang).ok_or_else(|| missing_data(lang).unwrap_or_default())?;
        let c_data = CString::new(data).unwrap();
        let c_lang = CString::new(lang).map_err(|_| "bad language".to_string())?;
        // SAFETY: plain C calls on a handle we own; strings outlive the calls.
        unsafe {
            let api = (f.create)();
            if api.is_null() { return Err("TessBaseAPICreate failed".into()); }
            if (f.init3)(api, c_data.as_ptr(), c_lang.as_ptr()) != 0 {
                (f.delete)(api);
                return Err(format!("Tesseract couldn't load '{lang}'"));
            }
            (f.set_psm)(api, PSM_SPARSE_TEXT);
            for (k, v) in [("debug_file", "/dev/null"), ("tessedit_do_invert", "1")] {
                let (k, v) = (CString::new(k).unwrap(), CString::new(v).unwrap());
                (f.set_variable)(api, k.as_ptr(), v.as_ptr());
            }
            Ok(Ocr { f, api })
        }
    }

    /// Recognise an 8-bit grayscale image (dark text on light works best).
    pub fn words(&mut self, gray: &[u8], width: u32, height: u32) -> Vec<Word> {
        assert_eq!(gray.len(), (width * height) as usize);
        let mut out = Vec::new();
        // SAFETY: `gray` lives across SetImage and Recognize (Tesseract copies it into a Pix
        // during SetImage anyway); the iterator is deleted before Clear.
        unsafe {
            let f = &self.f;
            (f.set_image)(self.api, gray.as_ptr(), width as c_int, height as c_int, 1, width as c_int);
            (f.set_resolution)(self.api, 300);
            if (f.recognize)(self.api, std::ptr::null_mut()) != 0 { (f.clear)(self.api); return out; }
            let it = (f.get_iterator)(self.api);
            if it.is_null() { (f.clear)(self.api); return out; }
            let page = (f.iter_page)(it);
            let mut line = 0u32;
            let mut first = true;
            loop {
                if !first && (f.page_begins)(page, RIL_TEXTLINE) != 0 { line += 1; }
                first = false;
                let raw = (f.iter_text)(it, RIL_WORD);
                if !raw.is_null() {
                    let text = CStr::from_ptr(raw).to_string_lossy().into_owned();
                    (f.delete_text)(raw);
                    let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
                    if (f.page_bbox)(page, RIL_WORD, &mut l, &mut t, &mut r, &mut b) != 0 {
                        let conf = (f.iter_conf)(it, RIL_WORD);
                        out.push(Word { text, conf, bbox: [l, t, r, b], line });
                    }
                }
                if (f.iter_next)(it, RIL_WORD) == 0 { break; }
            }
            (f.iter_delete)(it);
            (f.clear)(self.api);
        }
        out
    }
}

impl Drop for Ocr {
    fn drop(&mut self) {
        // SAFETY: the handle came from TessBaseAPICreate and is deleted once.
        unsafe { (self.f.delete)(self.api) }
    }
}
