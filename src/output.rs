use rustyline::ExternalPrinter;
use std::io::{self, Write};
use std::sync::Mutex;

pub trait OutputSink: Send + Sync {
    fn stdout(&self, text: &str);
    fn stderr(&self, text: &str);
    fn flush(&self);

    fn stdout_line(&self, text: &str) {
        self.stdout(text);
        self.stdout("\n");
    }

    fn stderr_line(&self, text: &str) {
        self.stderr(text);
        self.stderr("\n");
    }
}

#[derive(Default)]
pub struct StdOutputSink;

impl OutputSink for StdOutputSink {
    fn stdout(&self, text: &str) {
        let mut out = io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    fn stderr(&self, text: &str) {
        let mut err = io::stderr();
        let _ = err.write_all(text.as_bytes());
        let _ = err.flush();
    }

    fn flush(&self) {
        let _ = io::stdout().flush();
        let _ = io::stderr().flush();
    }
}

#[derive(Default)]
struct PrinterBuffers {
    stdout: String,
    stderr: String,
}

pub struct ExternalPrinterSink<P: ExternalPrinter + Send + 'static> {
    printer: Mutex<P>,
    buffers: Mutex<PrinterBuffers>,
}

impl<P: ExternalPrinter + Send + 'static> ExternalPrinterSink<P> {
    pub fn new(printer: P) -> Self {
        Self {
            printer: Mutex::new(printer),
            buffers: Mutex::new(PrinterBuffers::default()),
        }
    }

    fn write_buffered(&self, text: &str, is_stderr: bool) {
        let messages = {
            let mut buffers = match self.buffers.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            let buffer = if is_stderr {
                &mut buffers.stderr
            } else {
                &mut buffers.stdout
            };
            buffer.push_str(text);

            let mut out = Vec::new();
            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer.drain(..=newline_pos).collect::<String>();
                out.push(line);
            }
            out
        };

        for message in messages {
            self.print_message(message);
        }
    }

    fn flush_partial_buffers(&self) {
        let mut messages = Vec::new();
        {
            let mut buffers = match self.buffers.lock() {
                Ok(guard) => guard,
                Err(_) => return,
            };
            if !buffers.stdout.is_empty() {
                messages.push(std::mem::take(&mut buffers.stdout));
            }
            if !buffers.stderr.is_empty() {
                messages.push(std::mem::take(&mut buffers.stderr));
            }
        }
        for message in messages {
            self.print_message(message);
        }
    }

    fn print_message(&self, message: String) {
        if let Ok(mut printer) = self.printer.lock() {
            let _ = printer.print(message);
        }
    }
}

impl<P: ExternalPrinter + Send + 'static> OutputSink for ExternalPrinterSink<P> {
    fn stdout(&self, text: &str) {
        self.write_buffered(text, false);
    }

    fn stderr(&self, text: &str) {
        self.write_buffered(text, true);
    }

    fn flush(&self) {
        self.flush_partial_buffers();
    }
}
