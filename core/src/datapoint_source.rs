//! Datapoint sources for oracle-core
mod ada_usd;
mod erg_usd;
pub mod erg_xau;

use anyhow::anyhow;
use derive_more::From;
use futures::future::BoxFuture;
use thiserror::Error;

pub fn load_datapoint_source(
    predef_datapoint_source: Option<PredefinedDataPointSource>,
    custom_datapoint_source_shell_cmd: Option<String>,
) -> Result<Box<dyn DataPointSource>, anyhow::Error> {
    if let Some(external_script_name) = custom_datapoint_source_shell_cmd.clone() {
        Ok(Box::new(ExternalScript::new(external_script_name.clone())))
    } else {
        match predef_datapoint_source {
            Some(predef_datasource) => Ok(data_point_source_from_predef(predef_datasource)),
            _ => Err(anyhow!(
                "pool config data_point_source is empty along with data_point_source_custom_script in the oracle config"
            )),
        }
    }
}

fn data_point_source_from_predef(
    predef_datasource: PredefinedDataPointSource,
) -> Box<dyn DataPointSource> {
    match predef_datasource {
        PredefinedDataPointSource::NanoErgUsd => Box::new(NanoErgUsd),
        PredefinedDataPointSource::NanoErgXau => erg_xau_aggregator(),
        PredefinedDataPointSource::NanoAdaUsd => Box::new(NanoAdaUsd),
    }
}

pub trait DataPointSource: std::fmt::Debug {
    fn get_datapoint(&self) -> Result<i64, DataPointSourceError>;

    // fn get_datapoint_retry(&self, retries: u8) -> Result<i64, DataPointSourceError> {
    //     let mut last_error = None;
    //     for _ in 0..retries {
    //         match self.get_datapoint() {
    //             Ok(datapoint) => return Ok(datapoint),
    //             Err(err) => {
    //                 log::warn!("Failed to get datapoint from source: {}, retrying ...", err);
    //                 last_error = Some(err)
    //             }
    //         }
    //     }
    //     Err(last_error.unwrap())
    // }
}

pub trait DataPointFetcher: std::fmt::Debug {
    fn get_datapoint(&self) -> BoxFuture<'static, Result<i64, DataPointSourceError>>;
}

#[derive(Debug)]
pub struct DataPointSourceAggregator {
    pub fetchers: Vec<Box<dyn DataPointFetcher>>,
}

impl DataPointSourceAggregator {
    pub async fn fetch_datapoints_average(&self) -> Result<i64, DataPointSourceError> {
        let mut futures = Vec::new();
        for fetcher in &self.fetchers {
            futures.push(fetcher.get_datapoint());
        }
        let results = futures::future::join_all(futures).await;
        let ok_results: Vec<i64> = results.into_iter().flat_map(|res| res.ok()).collect();
        let average = ok_results.iter().sum::<i64>() / ok_results.len() as i64;
        Ok(average)
    }
}

impl DataPointSource for DataPointSourceAggregator {
    fn get_datapoint(&self) -> Result<i64, DataPointSourceError> {
        let tokio_runtime = tokio::runtime::Runtime::new().unwrap();
        tokio_runtime.block_on(self.fetch_datapoints_average())
    }
}

#[derive(Debug, From, Error)]
pub enum DataPointSourceError {
    #[error("external script error: {0}")]
    ExternalScript(ExternalScriptError),
    #[error("Reqwest error: {0}")]
    Reqwest(reqwest::Error),
    #[error("JSON parse error: {0}")]
    JsonParse(json::Error),
    #[error("Missing JSON field")]
    JsonMissingField,
}

#[derive(Debug, From, Error)]
pub enum ExternalScriptError {
    #[error("external script child process error: {0}")]
    ChildProcess(std::io::Error),
    #[error("String from bytes error: {0}")]
    StringFromBytes(std::string::FromUtf8Error),
    #[error("Parse i64 from string error: {0}")]
    ParseInt(std::num::ParseIntError),
}

#[derive(Debug, Clone)]
pub struct ExternalScript(String);

impl ExternalScript {
    pub fn new(script_name: String) -> Self {
        ExternalScript(script_name)
    }
}

impl DataPointSource for ExternalScript {
    fn get_datapoint(&self) -> Result<i64, DataPointSourceError> {
        let script_output = std::process::Command::new(&self.0)
            .output()
            .map_err(ExternalScriptError::from)?;
        let datapoint_str =
            String::from_utf8(script_output.stdout).map_err(ExternalScriptError::from)?;
        datapoint_str
            .parse()
            .map_err(|e| DataPointSourceError::from(ExternalScriptError::from(e)))
    }
}

pub use ada_usd::NanoAdaUsd;
pub use erg_usd::NanoErgUsd;

use self::erg_xau::erg_xau_aggregator;

#[derive(serde::Serialize, serde::Deserialize, Debug, Copy, Clone)]
#[allow(clippy::enum_variant_names)]
pub enum PredefinedDataPointSource {
    NanoErgUsd,
    NanoErgXau,
    NanoAdaUsd,
}
