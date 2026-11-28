//! `#[tauri::command]` leaves the function as it is: the GPUI client calls it directly.

use proc_macro::TokenStream;

#[proc_macro_attribute]
pub fn command(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
