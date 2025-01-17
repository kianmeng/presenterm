//! Code execution.

use crate::markdown::elements::{Code, CodeLanguage};
use std::{
    io::{self, BufRead, BufReader, Write},
    process::{self, ChildStdout, Stdio},
    sync::{Arc, Mutex},
    thread::{self},
};
use tempfile::NamedTempFile;

/// Allows executing code.
pub(crate) struct CodeExecuter;

impl CodeExecuter {
    /// Execute a piece of code.
    pub(crate) fn execute(code: &Code) -> Result<ExecutionHandle, CodeExecuteError> {
        if !code.language.supports_execution() {
            return Err(CodeExecuteError::UnsupportedExecution);
        }
        if !code.flags.execute {
            return Err(CodeExecuteError::NotExecutableCode);
        }
        match &code.language {
            CodeLanguage::Shell(interpreter) => Self::execute_shell(interpreter, &code.contents),
            _ => Err(CodeExecuteError::UnsupportedExecution),
        }
    }

    fn execute_shell(interpreter: &str, code: &str) -> Result<ExecutionHandle, CodeExecuteError> {
        let mut output_file = NamedTempFile::new().map_err(CodeExecuteError::TempFile)?;
        output_file.write_all(code.as_bytes()).map_err(CodeExecuteError::TempFile)?;
        output_file.flush().map_err(CodeExecuteError::TempFile)?;
        let process_handle = process::Command::new("/usr/bin/env")
            .arg(interpreter)
            .arg(output_file.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(CodeExecuteError::SpawnProcess)?;

        let state: Arc<Mutex<ExecutionState>> = Default::default();
        let reader_handle = ProcessReader::spawn(process_handle, state.clone(), output_file);
        let handle = ExecutionHandle { state, reader_handle };
        Ok(handle)
    }
}

/// An error during the execution of some code.
#[derive(thiserror::Error, Debug)]
pub(crate) enum CodeExecuteError {
    #[error("code language doesn't support execution")]
    UnsupportedExecution,

    #[error("code is not marked for execution")]
    NotExecutableCode,

    #[error("error creating temporary file: {0}")]
    TempFile(io::Error),

    #[error("error spawning process: {0}")]
    SpawnProcess(io::Error),
}

/// A handle for the execution of a piece of code.
#[derive(Debug)]
pub(crate) struct ExecutionHandle {
    state: Arc<Mutex<ExecutionState>>,
    #[allow(dead_code)]
    reader_handle: thread::JoinHandle<()>,
}

impl ExecutionHandle {
    /// Get the current state of the process.
    pub(crate) fn state(&self) -> ExecutionState {
        self.state.lock().unwrap().clone()
    }
}

/// Consumes the output of a process and stores it in a shared state.
struct ProcessReader {
    handle: process::Child,
    state: Arc<Mutex<ExecutionState>>,
    #[allow(dead_code)]
    file_handle: NamedTempFile,
}

impl ProcessReader {
    fn spawn(
        handle: process::Child,
        state: Arc<Mutex<ExecutionState>>,
        file_handle: NamedTempFile,
    ) -> thread::JoinHandle<()> {
        let reader = Self { handle, state, file_handle };
        thread::spawn(|| reader.run())
    }

    fn run(mut self) {
        let stdout = self.handle.stdout.take().expect("no stdout");
        let stdout = BufReader::new(stdout);
        let _ = Self::process_output(self.state.clone(), stdout);
        let success = match self.handle.try_wait() {
            Ok(Some(code)) => code.success(),
            _ => false,
        };
        let status = match success {
            true => ProcessStatus::Success,
            false => ProcessStatus::Failure,
        };
        self.state.lock().unwrap().status = status;
    }

    fn process_output(state: Arc<Mutex<ExecutionState>>, stdout: BufReader<ChildStdout>) -> io::Result<()> {
        for line in stdout.lines() {
            let line = line?;
            // TODO: consider not locking per line...
            state.lock().unwrap().output.push(line);
        }
        Ok(())
    }
}

/// The state of the execution of a process.
#[derive(Clone, Default, Debug)]
pub(crate) struct ExecutionState {
    pub(crate) output: Vec<String>,
    pub(crate) status: ProcessStatus,
}

/// The status of a process.
#[derive(Clone, Debug, Default)]
pub(crate) enum ProcessStatus {
    #[default]
    Running,
    Success,
    Failure,
}

impl ProcessStatus {
    /// Check whether the underlying process is finished.
    pub(crate) fn is_finished(&self) -> bool {
        matches!(self, ProcessStatus::Success | ProcessStatus::Failure)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::markdown::elements::CodeFlags;

    #[test]
    fn shell_code_execution() {
        let contents = r"
echo 'hello world'
echo 'bye'"
            .into();
        let code = Code { contents, language: CodeLanguage::Shell("sh".into()), flags: CodeFlags { execute: true } };
        let handle = CodeExecuter::execute(&code).expect("execution failed");
        let state = loop {
            let state = handle.state();
            if state.status.is_finished() {
                break state;
            }
        };

        let expected_lines = vec!["hello world", "bye"];
        assert_eq!(state.output, expected_lines);
    }

    #[test]
    fn non_executable_code_cant_be_executed() {
        let contents = String::new();
        let code = Code { contents, language: CodeLanguage::Shell("sh".into()), flags: CodeFlags { execute: false } };
        let result = CodeExecuter::execute(&code);
        assert!(result.is_err());
    }
}
