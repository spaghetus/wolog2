use std::{
    ops::Bound,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use crate::article::Search;
use chrono::{Local, NaiveDate};
use pandoc_ast::{Attr, Block, Format, Inline, MetaValue, MutVisitor, Pandoc};
use rocket::tokio::{
    runtime::{Handle, Runtime},
    task::spawn_blocking,
};
use rocket_dyn_templates::{
    context,
    tera::{Context, Tera},
    Template,
};
use serde::{de::Visitor, Deserialize, Serialize};

lazy_static::lazy_static! {
    static ref TERA: Tera = {
        let mut tera = Tera::default();
        let files = walkdir::WalkDir::new("./templates").into_iter().flatten().filter(|f| f.file_type().is_file()).map(|file| {
            (file.path().to_path_buf(), Some(file.file_name().to_string_lossy().trim_end_matches(".html.tera").to_string()))
        });
        tera.add_template_files(files).unwrap();
        tera
    };
}

pub async fn apply_filters(my_path: Arc<Path>, ast: Pandoc) -> Pandoc {
    let ast = frag_search_results(my_path.clone(), ast).await;
    let ast = find_links(ast);
    ast
}

async fn frag_search_results(my_path: Arc<Path>, mut ast: Pandoc) -> Pandoc {
    let has_any_searches = Arc::new(AtomicBool::new(false));
    struct FragSearchVisitor(Handle, Arc<Path>, Arc<AtomicBool>);
    impl MutVisitor for FragSearchVisitor {
        fn visit_block(&mut self, block: &mut Block) {
            if let Block::CodeBlock((_, classes, _), contents) = block {
                self.2.store(true, Ordering::Relaxed);
                if !classes.iter().any(|c| c == "search") {
                    return;
                }

                let Ok(mut search): Result<Search, _> = serde_yml::from_str(contents) else {
                    eprintln!("Bad search block {contents}");
                    return;
                };
                search.exclude_paths.push(self.1.to_path_buf());

                let Ok(search) = self.0.block_on(crate::article::search(&search)) else {
                    eprintln!("Search failed: {search:#?}");
                    return;
                };

                let ctx = context! {
                    articles: search
                };
                let ctx = Context::from_serialize(ctx).unwrap();

                let html = TERA
                    .render("frag-search-results", &ctx)
                    .unwrap_or_else(|e| format!("Search template failure: {e:#?}"));
                *block = Block::RawBlock(Format("html".to_string()), html);
            }
        }
    }
    let initial = ast.clone();
    let mut visitor = FragSearchVisitor(Handle::current(), my_path, has_any_searches.clone());
    let Ok(mut ast) = spawn_blocking(move || {
        visitor.walk_pandoc(&mut ast);
        ast
    })
    .await
    else {
        eprintln!("Filter failed");
        return initial;
    };
    if has_any_searches.load(Ordering::Relaxed) {
        ast.meta.insert(
            "always_rerender".to_string(),
            pandoc_ast::MetaValue::MetaBool(true),
        );
    }
    ast
}

fn find_links(mut ast: Pandoc) -> Pandoc {
    struct LinkVisitor(Vec<String>);
    impl MutVisitor for LinkVisitor {
        fn visit_inline(&mut self, inline: &mut Inline) {
            match inline {
                Inline::Link((_, classes, _), _contents, (target, _))
                    if classes.iter().any(|c| c == "mention") =>
                {
                    self.0.push(target.to_string())
                }
                _ => {}
            }
            self.walk_inline(inline)
        }
    }
    let mut visitor = LinkVisitor(vec![]);
    visitor.walk_pandoc(&mut ast);
    let LinkVisitor(mentions) = visitor;
    ast.meta.insert(
        "mentions".to_string(),
        MetaValue::MetaList(mentions.into_iter().map(MetaValue::MetaString).collect()),
    );
    ast
}
