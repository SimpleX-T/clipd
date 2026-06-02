//! Install the palette's custom CSS. The actual rules live in `style.css`
//! and are baked into the binary at compile time.

use gtk::gdk;

const CSS: &str = include_str!("style.css");

pub fn install() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
