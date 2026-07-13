use memd::cli;
use memd::mcp;
use memd::MemoryStore;

#[test]
fn cli_public_paths_resolve() {
    let _ = cli::run_cli::<MemoryStore>;
    let _ = cli::run_warm_admin;
    let _ = cli::try_run_warm_client;
    let _ = cli::warm_socket_path;

    let _ = std::any::type_name::<cli::CliCommand>();
    let _ = std::any::type_name::<cli::WarmCommand>();
    let _ = std::any::type_name::<cli::WarmMode>();
    let _ = std::any::type_name::<cli::WarmProcessConfig>();
    let _ = std::any::type_name::<cli::ExportFormat>();
    let _ = std::any::type_name::<cli::CliQueryMode>();
    let _ = std::any::type_name::<cli::SearchReranker>();
}

#[test]
fn mcp_compatibility_exports_resolve() {
    let _ = mcp::configure_operation_routing;
    let _ = mcp::handle_memory_search::<MemoryStore>;

    let _ = std::any::type_name::<mcp::McpError>();
    let _ = std::any::type_name::<mcp::PostWriteEvent>();
    let _ = std::any::type_name::<mcp::QueryMode>();
    let _ = std::any::type_name::<mcp::SearchParams>();
    let _ = std::any::type_name::<mcp::ArtifactCreateParams>();
    let _ = std::any::type_name::<mcp::TaskStartParams>();
    let _ = std::any::type_name::<mcp::dedup::ResolvedDedup>();
    let _ = std::any::type_name::<mcp::markdown_export::RenderedFile>();
    let _ = std::any::type_name::<mcp::post_write_hooks::PostWriteEvent>();
}

#[test]
fn protocol_neutral_operation_paths_resolve() {
    let _ = std::any::type_name::<memd::ops::OperationError>();
    let _ = std::any::type_name::<memd::dedup::ResolvedDedup>();
    let _ = std::any::type_name::<memd::markdown_export::RenderedFile>();
    let _ = std::any::type_name::<memd::post_write_hooks::PostWriteEvent>();
}
