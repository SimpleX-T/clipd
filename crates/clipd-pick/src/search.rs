//! Result list types. With the new tabbed UI, clipboard and emoji
//! results render in separate views — no more interleaved ranking.

use clipd_proto::Clip;

use crate::emoji::EmojiRef;

#[derive(Debug, Clone)]
pub enum Result_ {
    Clip(Clip),
    Emoji(EmojiRef),
}
