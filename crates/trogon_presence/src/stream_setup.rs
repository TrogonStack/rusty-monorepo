use async_nats::jetstream::context::{CreateStreamError, CreateStreamErrorKind, GetStreamError, GetStreamErrorKind};
use async_nats::jetstream::{self, stream, ErrorCode};

use crate::bucket::BucketField;

pub type StreamCheck = fn(&stream::Config, &stream::Config) -> Result<(), BucketField>;

#[derive(Debug, Clone)]
pub struct StreamSpec {
    config: stream::Config,
    check: StreamCheck,
}

#[derive(Debug, thiserror::Error)]
pub enum StreamSetupError {
    #[error("stream {stream} does not exist; provision it first")]
    Missing { stream: String },
    #[error("stream {stream} has an incompatible {field}")]
    Incompatible { stream: String, field: BucketField },
    #[error("looking up stream {stream}: {source}")]
    Lookup { stream: String, source: GetStreamError },
    #[error("creating stream {stream}: {source}")]
    Create { stream: String, source: CreateStreamError },
}

impl StreamSpec {
    pub fn new(config: stream::Config, check: StreamCheck) -> Self {
        Self { config, check }
    }

    pub fn config(&self) -> &stream::Config {
        &self.config
    }

    pub async fn create_or_verify(&self, context: &jetstream::Context) -> Result<stream::Stream, StreamSetupError> {
        match context.get_stream(&self.config.name).await {
            Ok(stream) => self.verified(stream),
            Err(source) if is_stream_missing(&source) => match context.create_stream(self.config.clone()).await {
                Ok(stream) => Ok(stream),
                Err(source) if is_name_taken(&source) => self.open(context).await,
                Err(source) => Err(StreamSetupError::Create {
                    stream: self.config.name.clone(),
                    source,
                }),
            },
            Err(source) => Err(StreamSetupError::Lookup {
                stream: self.config.name.clone(),
                source,
            }),
        }
    }

    pub async fn open(&self, context: &jetstream::Context) -> Result<stream::Stream, StreamSetupError> {
        match context.get_stream(&self.config.name).await {
            Ok(stream) => self.verified(stream),
            Err(source) if is_stream_missing(&source) => Err(StreamSetupError::Missing {
                stream: self.config.name.clone(),
            }),
            Err(source) => Err(StreamSetupError::Lookup {
                stream: self.config.name.clone(),
                source,
            }),
        }
    }

    fn verified(&self, stream: stream::Stream) -> Result<stream::Stream, StreamSetupError> {
        (self.check)(&self.config, &stream.cached_info().config).map_err(|field| StreamSetupError::Incompatible {
            stream: self.config.name.clone(),
            field,
        })?;
        Ok(stream)
    }
}

fn is_stream_missing(error: &GetStreamError) -> bool {
    matches!(
        error.kind(),
        GetStreamErrorKind::JetStream(ref err) if err.error_code() == ErrorCode::STREAM_NOT_FOUND
    )
}

fn is_name_taken(error: &CreateStreamError) -> bool {
    matches!(
        error.kind(),
        CreateStreamErrorKind::JetStream(ref err) if err.error_code() == ErrorCode::STREAM_NAME_EXIST
    )
}
