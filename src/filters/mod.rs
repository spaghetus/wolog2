use pandoc_ast::Pandoc;

pub const FILTERS: &[&dyn Fn(&mut Pandoc) -> String] = &[];
