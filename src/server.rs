use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::request::*;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use crate::call_hierarchy::{incoming_calls, outgoing_calls, prepare_call_hierarchy};
use crate::completion::compute_completions;
use crate::definition::{
    compute_declaration, compute_definition, compute_implementation, compute_type_definition,
};
use crate::diagnostics::analyze_document;
use crate::document::DocumentStore;
use crate::format::compute_formatting;
use crate::hover::compute_hover;
use crate::inlay_hints::compute_inlay_hints;
use crate::references::compute_references;
use crate::signature_help::compute_signature_help;
use crate::symbols::compute_document_symbols;
use crate::workspace::WorkspaceEnv;

pub struct YukinoLanguageServer {
    client: Client,
    documents: DocumentStore,
    environments: Arc<RwLock<HashMap<PathBuf, Arc<WorkspaceEnv>>>>,
    build_lock: Arc<tokio::sync::Mutex<()>>,
}

impl YukinoLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DocumentStore::new(),
            environments: Arc::new(RwLock::new(HashMap::new())),
            build_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn get_env_for_file(&self, file_path: &Path) -> Arc<WorkspaceEnv> {
        let (root, _) = crate::workspace::find_project_root_and_manifest(file_path);
        if let Ok(lock) = self.environments.read() {
            if let Some(env) = lock.get(&root) {
                return Arc::clone(env);
            }
        }
        let env = Arc::new(WorkspaceEnv::for_path(file_path));
        if let Ok(mut lock) = self.environments.write() {
            lock.insert(root, Arc::clone(&env));
        }
        env
    }

    async fn revalidate(&self, uri: &Url) {
        if let Some(doc) = self.documents.get(uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            if file_path.components().any(|c| c.as_os_str() == "yuki-work") {
                self.client.publish_diagnostics(uri.clone(), Vec::new(), Some(doc.version)).await;
                return;
            }
            let env = self.get_env_for_file(&file_path);
            let (_, diags) = analyze_document(&doc, Some(&env));
            self.client.publish_diagnostics(uri.clone(), diags, Some(doc.version)).await;
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for YukinoLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                        ..Default::default()
                    },
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        ":".to_string(),
                        "#".to_string(),
                        " ".to_string(),
                    ]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                declaration_provider: Some(DeclarationCapability::Simple(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![
                        ",".to_string(),
                        ")".to_string(),
                        " ".to_string(),
                    ]),
                    work_done_progress_options: Default::default(),
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "yukino-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        tracing::info!("yukino-lsp initialized and ready");
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        let version = params.text_document.version;

        self.documents.insert(uri.clone(), text, version);
        self.revalidate(&uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;

        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents.insert(uri.clone(), change.text, version);
            self.revalidate(&uri).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents.remove(&params.text_document.uri);
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        let file_path = uri.to_file_path().unwrap_or_default();
        let (project_root, manifest_opt) = crate::workspace::find_project_root_and_manifest(&file_path);

        if manifest_opt.is_present() {
            let _permit = self.build_lock.lock().await;

            let _ = tokio::process::Command::new("yukino")
                .arg("build")
                .current_dir(&project_root)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await;

            let fresh_env = Arc::new(WorkspaceEnv::for_path(&file_path));
            if let Ok(mut lock) = self.environments.write() {
                lock.insert(project_root.clone(), Arc::clone(&fresh_env));
            }

            for open_uri in self.documents.all_uris() {
                let open_path = open_uri.to_file_path().unwrap_or_default();
                let (open_root, _) = crate::workspace::find_project_root_and_manifest(&open_path);
                if open_root == project_root {
                    self.revalidate(&open_uri).await;
                }
            }
        } else {
            self.revalidate(&uri).await;
        }
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(Some(compute_completions(&doc, pos, &module, &env)));
        }

        Ok(None)
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_hover(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_document_symbols(&doc, &module));
        }

        Ok(None)
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_definition(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_declaration(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_type_definition(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_implementation(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(Some(compute_references(
                &doc,
                pos,
                &module,
                &env,
                params.context.include_declaration,
            )));
        }

        Ok(None)
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(compute_signature_help(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(Some(compute_inlay_hints(&doc, params.range, &module, &env)));
        }

        Ok(None)
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        if let Some(doc) = self.documents.get(&uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(prepare_call_hierarchy(&doc, pos, &module, &env));
        }

        Ok(None)
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let uri = &params.item.uri;

        if let Some(doc) = self.documents.get(uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(Some(incoming_calls(params.item, &module, &doc)));
        }

        Ok(None)
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let uri = &params.item.uri;

        if let Some(doc) = self.documents.get(uri) {
            let file_path = uri.to_file_path().unwrap_or_default();
            let env = self.get_env_for_file(&file_path);
            let (module, _) = analyze_document(&doc, Some(&env));
            return Ok(Some(outgoing_calls(params.item, &module, &doc)));
        }

        Ok(None)
    }

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri;

        if let Some(doc) = self.documents.get(&uri) {
            return Ok(compute_formatting(&doc));
        }

        Ok(None)
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_initialize_sync_capabilities() {
        let (service, _) = tower_lsp::LspService::new(YukinoLanguageServer::new);
        let server = service.inner();
        let res = server.initialize(InitializeParams::default()).await.unwrap();
        let sync = res.capabilities.text_document_sync.expect("text_document_sync capability");
        match sync {
            TextDocumentSyncCapability::Options(opts) => {
                assert_eq!(opts.open_close, Some(true));
                assert_eq!(opts.change, Some(TextDocumentSyncKind::FULL));
                assert!(matches!(opts.save, Some(TextDocumentSyncSaveOptions::Supported(true))));
            }
            _ => panic!("expected TextDocumentSyncCapability::Options"),
        }
    }
}
