//! Paragraph layout for the egui widget: shaping text with egui's fonts and
//! indexing the shaped paragraphs by position.

mod index;
mod layout_cache;

pub(super) use index::{LayoutIdentity, ParagraphLayouts};
pub(crate) use layout_cache::ParagraphLayoutView;
#[cfg(test)]
pub(super) use layout_cache::paragraph_job;
pub(super) use layout_cache::{
    Appearance, LayoutCache, ParagraphCache, ParagraphLayout, is_layout_text,
};
