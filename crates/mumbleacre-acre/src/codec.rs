use thiserror::Error;

/// ACRE2's `TEXTMESSAGE_BUFSIZE` in v2.14.0.1064, including the NUL byte when
/// a producer writes one to the named pipe.
pub const ACRE_MAX_MESSAGE_BYTES: usize = 4096;

/// ACRE2's `TEXTMESSAGE_MAX_PARAMETER_COUNT` in v2.14.0.1064.
pub const ACRE_MAX_PARAMETER_COUNT: usize = 1024;

/// One private ACRE RPC message.
///
/// The protocol uses ASCII text in the form `procedure:param1,param2,`.  It
/// has no escaping mechanism, so commas cannot occur inside a parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcreMessage {
    procedure: String,
    parameters: Vec<String>,
    trailing_comma: bool,
}

impl AcreMessage {
    /// Creates an RPC message without a trailing comma.
    pub fn new(
        procedure: impl Into<String>,
        parameters: Vec<String>,
    ) -> Result<Self, AcreCodecError> {
        Self::with_trailing_comma(procedure, parameters, false)
    }

    /// Creates an RPC message with explicit control of the optional trailing
    /// comma used by ACRE's C++ formatter.
    pub fn with_trailing_comma(
        procedure: impl Into<String>,
        parameters: Vec<String>,
        trailing_comma: bool,
    ) -> Result<Self, AcreCodecError> {
        let procedure = procedure.into();
        validate_procedure(&procedure)?;
        validate_parameters(&parameters)?;

        let message = Self {
            procedure,
            parameters,
            trailing_comma,
        };
        message.validate_encoded_length()?;
        Ok(message)
    }

    /// Parses a message received from either ACRE pipe direction.
    ///
    /// The ACRE extension writes inbound messages without a NUL terminator,
    /// while the historical `TeamSpeak` plugin writes outbound messages with
    /// one.  Both forms are accepted; bytes after a NUL are rejected so an
    /// invalid frame cannot be partially interpreted.
    pub fn parse(wire: &[u8]) -> Result<Self, AcreCodecError> {
        if wire.is_empty() {
            return Err(AcreCodecError::EmptyMessage);
        }
        if wire.len() > ACRE_MAX_MESSAGE_BYTES {
            return Err(AcreCodecError::MessageTooLong {
                length: wire.len(),
                limit: ACRE_MAX_MESSAGE_BYTES,
            });
        }

        let text = match wire.iter().position(|byte| *byte == 0) {
            Some(nul_index) => {
                if wire[nul_index + 1..].iter().any(|byte| *byte != 0) {
                    return Err(AcreCodecError::DataAfterNul);
                }
                &wire[..nul_index]
            }
            None => wire,
        };
        if text.is_empty() {
            return Err(AcreCodecError::EmptyMessage);
        }
        if let Some((offset, byte)) = text
            .iter()
            .copied()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii())
        {
            return Err(AcreCodecError::NonAscii { offset, byte });
        }

        let text = std::str::from_utf8(text)
            .map_err(|_| AcreCodecError::NonAscii { offset: 0, byte: 0 })?;
        let Some((procedure, raw_parameters)) = text.split_once(':') else {
            return Err(AcreCodecError::MissingDelimiter);
        };
        validate_procedure(procedure)?;

        let trailing_comma = raw_parameters.ends_with(',');
        let raw_parameters = raw_parameters.strip_suffix(',').unwrap_or(raw_parameters);
        let parameters = if raw_parameters.is_empty() {
            Vec::new()
        } else {
            raw_parameters.split(',').map(ToOwned::to_owned).collect()
        };
        validate_parameters(&parameters)?;

        Ok(Self {
            procedure: procedure.to_owned(),
            parameters,
            trailing_comma,
        })
    }

    #[must_use]
    pub fn procedure(&self) -> &str {
        &self.procedure
    }

    #[must_use]
    pub fn parameters(&self) -> &[String] {
        &self.parameters
    }

    #[must_use]
    pub fn trailing_comma(&self) -> bool {
        self.trailing_comma
    }

    /// Returns the ASCII representation without a NUL byte.
    #[must_use]
    pub fn encode_text(&self) -> String {
        let mut text = String::with_capacity(self.encoded_len_without_nul());
        text.push_str(&self.procedure);
        text.push(':');
        for (index, parameter) in self.parameters.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(parameter);
        }
        if self.trailing_comma {
            text.push(',');
        }
        text
    }

    /// Encodes the message with the NUL terminator emitted by ACRE's stock
    /// voice plugin when it writes back to `ACRE2Arma`.
    #[must_use]
    pub fn encode_nul_terminated(&self) -> Vec<u8> {
        let mut bytes = self.encode_text().into_bytes();
        bytes.push(0);
        bytes
    }

    #[must_use]
    pub fn encoded_len_without_nul(&self) -> usize {
        let separators = self.parameters.len().saturating_sub(1);
        self.procedure.len()
            + 1 // ':'
            + self.parameters.iter().map(String::len).sum::<usize>()
            + separators
            + usize::from(self.trailing_comma)
    }

    fn validate_encoded_length(&self) -> Result<(), AcreCodecError> {
        let length = self.encoded_len_without_nul() + 1;
        if length > ACRE_MAX_MESSAGE_BYTES {
            return Err(AcreCodecError::MessageTooLong {
                length,
                limit: ACRE_MAX_MESSAGE_BYTES,
            });
        }
        Ok(())
    }
}

fn validate_procedure(procedure: &str) -> Result<(), AcreCodecError> {
    if procedure.len() < 2 {
        return Err(AcreCodecError::ProcedureTooShort);
    }
    if !procedure
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(AcreCodecError::InvalidProcedure);
    }
    Ok(())
}

fn validate_parameters(parameters: &[String]) -> Result<(), AcreCodecError> {
    if parameters.len() > ACRE_MAX_PARAMETER_COUNT {
        return Err(AcreCodecError::TooManyParameters {
            count: parameters.len(),
            limit: ACRE_MAX_PARAMETER_COUNT,
        });
    }

    for (index, parameter) in parameters.iter().enumerate() {
        if let Some(byte) = parameter
            .bytes()
            .find(|byte| !byte.is_ascii() || *byte == b',' || *byte == 0 || byte.is_ascii_control())
        {
            return Err(AcreCodecError::InvalidParameter { index, byte });
        }
    }
    Ok(())
}

/// Validation error for the bounded ACRE wire format.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AcreCodecError {
    #[error("ACRE RPC message is empty")]
    EmptyMessage,
    #[error("ACRE RPC message is {length} bytes; limit is {limit}")]
    MessageTooLong { length: usize, limit: usize },
    #[error("ACRE RPC message contains non-ASCII byte {byte:#x} at offset {offset}")]
    NonAscii { offset: usize, byte: u8 },
    #[error("ACRE RPC message has non-NUL bytes after its terminator")]
    DataAfterNul,
    #[error("ACRE RPC message has no procedure delimiter")]
    MissingDelimiter,
    #[error("ACRE RPC procedure must contain at least two characters")]
    ProcedureTooShort,
    #[error("ACRE RPC procedure contains unsupported characters")]
    InvalidProcedure,
    #[error("ACRE RPC message has {count} parameters; limit is {limit}")]
    TooManyParameters { count: usize, limit: usize },
    #[error("ACRE RPC parameter {index} contains unsupported byte {byte:#x}")]
    InvalidParameter { index: usize, byte: u8 },
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn parses_the_forms_emitted_by_acre2_arma_and_the_stock_plugin() {
        let without_nul = AcreMessage::parse(b"getClientID:2:1234,").unwrap();
        assert_eq!(without_nul.procedure(), "getClientID");
        assert_eq!(without_nul.parameters(), ["2:1234"]);
        assert!(without_nul.trailing_comma());

        let with_nul = AcreMessage::parse(b"handleGetPluginVersion:2.14.0.1064\0").unwrap();
        assert_eq!(with_nul.procedure(), "handleGetPluginVersion");
        assert_eq!(with_nul.parameters(), ["2.14.0.1064"]);
        assert!(!with_nul.trailing_comma());
    }

    #[test]
    fn preserves_empty_interior_parameters_and_a_final_comma() {
        let message = AcreMessage::parse(b"setSetting:radio,,value,").unwrap();
        assert_eq!(message.parameters(), ["radio", "", "value"]);
        assert!(message.trailing_comma());
        assert_eq!(message.encode_text(), "setSetting:radio,,value,");
    }

    #[test]
    fn serializes_a_stock_plugin_style_reply() {
        let message = AcreMessage::with_trailing_comma(
            "handleGetClientID",
            vec!["42".to_owned(), "2:1234".to_owned()],
            true,
        )
        .unwrap();
        assert_eq!(
            message.encode_nul_terminated(),
            b"handleGetClientID:42,2:1234,\0"
        );
    }

    #[test]
    fn rejects_partial_or_oversized_messages() {
        assert_eq!(
            AcreMessage::parse(b"ping:\0ignored").unwrap_err(),
            AcreCodecError::DataAfterNul
        );
        assert!(matches!(
            AcreMessage::parse(&[b'x'; ACRE_MAX_MESSAGE_BYTES + 1]),
            Err(AcreCodecError::MessageTooLong { .. })
        ));
        assert!(matches!(
            AcreMessage::parse(b"ping:\xff"),
            Err(AcreCodecError::NonAscii { .. })
        ));
    }

    #[test]
    fn formatted_error_is_useful_without_leaking_payloads() {
        let error = AcreMessage::new("x", Vec::new()).unwrap_err();
        let mut rendered = String::new();
        write!(&mut rendered, "{error}").unwrap();
        assert!(rendered.contains("at least two"));
    }
}
