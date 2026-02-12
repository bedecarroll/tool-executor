use std::io::{Error, ErrorKind, Write};

#[test]
fn command_factory_returns_tx() {
    let cmd = tool_executor::command();
    assert_eq!(cmd.get_name(), "tx");
}

#[test]
fn write_cli_error_renders_chain() -> color_eyre::Result<()> {
    let err = color_eyre::eyre::eyre!("root")
        .wrap_err("middle")
        .wrap_err("top");
    let mut output = Vec::new();
    tool_executor::write_cli_error(&err, &mut output)?;
    let text = String::from_utf8(output)?;
    assert!(text.contains("tx: top"));
    assert!(text.contains("caused by: middle"));
    assert!(text.contains("caused by: root"));
    Ok(())
}

struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(Error::new(ErrorKind::BrokenPipe, "boom"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_cli_error_propagates_writer_failure() {
    let err = color_eyre::eyre::eyre!("root");
    let write_err = tool_executor::write_cli_error(&err, FailingWriter).unwrap_err();
    assert_eq!(write_err.kind(), ErrorKind::BrokenPipe);
}
