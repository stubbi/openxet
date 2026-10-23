use std::io::{self, Write};

use serde::Serialize;

/// Human-readable rendering of a command result.
pub trait HumanDisplay {
    fn human(&self) -> String;
}

/// How a command prints its result: for people, or as JSON for agents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Human,
    Json,
}

impl Output {
    pub fn from_json_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Human }
    }

    /// Writes `value` to stdout, followed by a newline.
    pub fn emit<T: Serialize + HumanDisplay>(self, value: &T) -> io::Result<()> {
        self.write_to(&mut io::stdout().lock(), value)
    }

    fn write_to<T: Serialize + HumanDisplay>(
        self,
        out: &mut impl Write,
        value: &T,
    ) -> io::Result<()> {
        match self {
            Self::Human => writeln!(out, "{}", value.human()),
            Self::Json => {
                serde_json::to_writer(&mut *out, value)?;
                writeln!(out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Sample {
        schema: &'static str,
        n: u8,
    }

    impl HumanDisplay for Sample {
        fn human(&self) -> String {
            format!("n is {}", self.n)
        }
    }

    const SAMPLE: Sample = Sample {
        schema: "openxet.sample.v1",
        n: 7,
    };

    fn render(output: Output) -> String {
        let mut buf = Vec::new();
        output.write_to(&mut buf, &SAMPLE).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn human_uses_the_human_display() {
        assert_eq!(render(Output::Human), "n is 7\n");
    }

    #[test]
    fn json_is_one_line_of_the_serialized_value() {
        assert_eq!(
            render(Output::Json),
            "{\"schema\":\"openxet.sample.v1\",\"n\":7}\n"
        );
    }
}
