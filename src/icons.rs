//! tlum's few icons, as SVG bodies on a 24-point grid, drawn in their element's colour.

use rune::Icon;

const fn stroke(name: &'static str, body: &'static str) -> Icon { Icon { name, view: 24.0, body } }

pub static GEAR: Icon = stroke("tlum-gear", r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="6.2"/><circle cx="12" cy="12" r="2.4"/></g><g stroke="currentColor" stroke-width="3.2" stroke-linecap="round"><path d="M12 2.6v2.2M12 19.2v2.2M2.6 12h2.2M19.2 12h2.2M5.35 5.35l1.55 1.55M17.1 17.1l1.55 1.55M5.35 18.65l1.55-1.55M17.1 6.9l1.55-1.55"/></g>"#);

pub static BOOK: Icon = stroke("tlum-book", r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"><path d="M3 5.5c2.6-1.1 5.6-1 9 1 3.4-2 6.4-2.1 9-1V19c-2.6-1.1-5.6-1-9 1-3.4-2-6.4-2.1-9-1z"/><path d="M12 6.5V20"/></g>"#);

pub static PALETTE: Icon = stroke("tlum-palette", r#"<path fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" d="M12 3a9 9 0 1 0 0 18c1.1 0 1.8-.8 1.8-1.8 0-.5-.2-.9-.5-1.2-.3-.3-.5-.7-.5-1.2 0-1 .8-1.8 1.8-1.8H17a4 4 0 0 0 4-4c0-4.4-4-8-9-8z"/><g fill="currentColor"><circle cx="7.5" cy="11.5" r="1.5"/><circle cx="10" cy="7.3" r="1.5"/><circle cx="14.8" cy="7.3" r="1.5"/></g>"#);

pub static SLIDERS: Icon = stroke("tlum-sliders", r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M4 7h9M19 7h1M4 17h3M13 17h7"/><circle cx="16" cy="7" r="2.6"/><circle cx="10" cy="17" r="2.6"/></g>"#);

pub static GRIP: Icon = stroke("tlum-grip", r#"<g fill="currentColor"><circle cx="9" cy="6" r="1.6"/><circle cx="15" cy="6" r="1.6"/><circle cx="9" cy="12" r="1.6"/><circle cx="15" cy="12" r="1.6"/><circle cx="9" cy="18" r="1.6"/><circle cx="15" cy="18" r="1.6"/></g>"#);

pub static TRASH: Icon = stroke("tlum-trash", r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3"/></g>"#);

pub static DOWNLOAD: Icon = stroke("tlum-download", r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4v11M7 10l5 5 5-5M5 20h14"/></g>"#);

pub static SEARCH: Icon = stroke("tlum-search", r#"<g fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="10.5" cy="10.5" r="6.5"/><path d="M15.5 15.5L20 20"/></g>"#);

pub static CHECK: Icon = stroke("tlum-check", r#"<path fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" d="M5 12.5l4.5 4.5L19 7.5"/>"#);

pub static PLUS: Icon = stroke("tlum-plus", r#"<path fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" d="M12 5v14M5 12h14"/>"#);

pub static FOLDER: Icon = stroke("tlum-folder", r#"<path fill="none" stroke="currentColor" stroke-width="2" stroke-linejoin="round" d="M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2.5h8.5A1.5 1.5 0 0 1 21 9v9.5a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 18.5z"/>"#);
