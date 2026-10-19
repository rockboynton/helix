//! Fetch semantic colors off the rendering path, guarded by document version,
//! server identity and cancellation generation. Tree-sitter remains the fallback.
use helix_core::syntax::config::LanguageServerFeature;
use helix_event::{cancelable_future, register_hook, AsyncHook};
use helix_lsp::{lsp, semantic_tokens, util::pos_to_lsp_pos, LanguageServerId};
use helix_view::{
    events::{
        ConfigDidChange, DocumentDidChange, DocumentDidOpen, LanguageServerExited,
        LanguageServerInitialized,
    },
    DocumentId, Editor,
};
use std::{collections::HashSet, time::Duration};
use tokio::time::Instant;

#[derive(Default)]
struct Handler(HashSet<DocumentId>);
impl AsyncHook for Handler {
    type Event = DocumentId;
    fn handle_event(&mut self, doc: DocumentId, _: Option<Instant>) -> Option<Instant> {
        self.0.insert(doc);
        Some(Instant::now() + Duration::from_millis(100))
    }
    fn finish_debounce(&mut self) {
        let docs = std::mem::take(&mut self.0);
        crate::job::dispatch_blocking(move |editor, _| {
            for doc in docs {
                request(editor, doc);
            }
        });
    }
}

fn request(editor: &mut Editor, id: DocumentId) {
    let enabled = editor.config().lsp.enable && editor.config().lsp.semantic_tokens;
    let Some(doc) = editor.document_mut(id) else {
        return;
    };
    let cancel = doc.semantic_token_controller.restart();
    if !enabled {
        doc.semantic_highlights.clear();
        return;
    }
    let Some(server) = doc
        .language_servers_with_feature(LanguageServerFeature::SemanticTokens)
        .next()
    else {
        doc.semantic_highlights.clear();
        return;
    };
    let version = doc.version();
    let text = doc.text().clone();
    let identifier = doc.identifier();
    let encoding = server.offset_encoding();
    let range = lsp::Range::new(
        lsp::Position::new(0, 0),
        pos_to_lsp_pos(&text, text.len_chars(), encoding),
    );
    let legend = semantic_tokens::options(server.capabilities())
        .unwrap()
        .legend
        .clone();
    let server_id = server.id();
    let future = server.semantic_tokens(identifier, range);
    tokio::spawn(async move {
        let tokens = match cancelable_future(future, &cancel).await {
            Some(Ok(tokens)) => tokens,
            Some(Err(error)) => {
                log::debug!("semantic token request failed: {error}");
                return;
            }
            None => return,
        };
        let highlights = semantic_tokens::decode(&text, &tokens, &legend, encoding);
        crate::job::dispatch(move |editor, _| {
            if cancel.is_canceled() || !editor.config().lsp.semantic_tokens {
                return;
            }
            let Some(doc) = editor.document_mut(id) else {
                return;
            };
            let active_server = doc
                .language_servers_with_feature(LanguageServerFeature::SemanticTokens)
                .next()
                .map(|server| server.id());
            if doc.version() == version && active_server == Some(server_id) {
                doc.semantic_highlights = highlights;
            }
        })
        .await;
    });
}

pub(crate) fn refresh(editor: &mut Editor, server: Option<LanguageServerId>) {
    let docs = editor
        .documents()
        .filter(|doc| server.is_none_or(|id| doc.supports_language_server(id)))
        .map(|doc| doc.id())
        .collect::<Vec<_>>();
    for doc in docs {
        request(editor, doc);
    }
}

pub(super) fn register_hooks() {
    let tx = Handler::default().spawn();
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        request(event.editor, event.doc);
        Ok(())
    });
    register_hook!(move |event: &mut DocumentDidChange<'_>| {
        // Never display an old server snapshot at new text positions. Syntax
        // colors are still immediate while the replacement request is debounced.
        event.doc.semantic_token_controller.cancel();
        event.doc.semantic_highlights.clear();
        if !event.ghost_transaction
            && event.doc.config.load().lsp.enable
            && event.doc.config.load().lsp.semantic_tokens
        {
            helix_event::send_blocking(&tx, event.doc.id());
        }
        Ok(())
    });
    register_hook!(move |event: &mut LanguageServerInitialized<'_>| {
        refresh(event.editor, None);
        Ok(())
    });
    register_hook!(move |event: &mut LanguageServerExited<'_>| {
        for doc in event.editor.documents_mut() {
            doc.semantic_token_controller.cancel();
            doc.semantic_highlights.clear();
        }
        refresh(event.editor, None);
        Ok(())
    });
    register_hook!(move |event: &mut ConfigDidChange<'_>| {
        refresh(event.editor, None);
        Ok(())
    });
}
