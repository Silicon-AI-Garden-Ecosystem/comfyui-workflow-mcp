use std::process::ExitCode;

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("failed to start the tokio runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(comfyui_workflow_mcp::startup::run())
}
