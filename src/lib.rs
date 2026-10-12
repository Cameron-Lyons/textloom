#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod accessibility;
pub mod adapter;
mod document;
mod editor;
mod export;
mod fragment;
mod search;
mod types;

pub use document::{Document, Paragraph, Span};
pub use editor::{Composition, Editor, HistoryLimits};
pub use fragment::{DecodeLimits, Fragment, FragmentError};
pub use search::SearchOptions;
pub use types::{
    Color, Error, InlineAttribute, InlineStyle, Link, Movement, ParagraphKind, Position, Selection,
    SelectionStyle, StylePatch,
};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Editor>();
    assert_send_sync::<Document>();
    assert_send_sync::<Paragraph>();
    assert_send_sync::<Fragment>();
    assert_send_sync::<Composition>();
    assert_send_sync::<Link>();
    assert_send_sync::<Error>();
    assert_send_sync::<FragmentError>();
};
