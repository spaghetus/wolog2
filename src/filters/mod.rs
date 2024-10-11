use crate::article::ArticleManager;
use pandoc_ast::Pandoc;

pub const FILTERS: &[&dyn Fn(&mut Pandoc, &ArticleManager) -> String] = &[];
