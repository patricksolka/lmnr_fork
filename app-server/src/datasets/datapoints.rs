use std::{
    collections::HashMap,
    io::{BufReader, Cursor},
    sync::Arc,
};

use anyhow::Result;
use csv;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{
    db::{self, datapoints::DBDatapoint, DB},
    pipeline::nodes::NodeInput,
    semantic_search::{
        semantic_search_grpc::index_request::Datapoint as VectorDBDatapoint,
        utils::merge_chat_messages,
    },
    traces::utils::json_value_to_string,
};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Datapoint {
    pub id: Uuid,
    pub dataset_id: Uuid,
    pub data: Value,
    pub target: Option<Value>,
    pub metadata: HashMap<String, Value>,
}

impl Datapoint {
    pub fn try_from_raw_value(dataset_id: Uuid, raw: &Value) -> Option<Self> {
        match raw {
            Value::Object(raw_obj) => {
                // Checks that the object has a `data` field and optionally a `target` field
                // and no other fields
                let data = raw_obj.get("data");
                let id = raw_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .unwrap_or(Uuid::new_v4());
                if data.is_some()
                    && raw_obj
                        .keys()
                        .all(|k| matches!(k.as_str(), "data" | "target" | "metadata" | "id"))
                {
                    let metadata = serde_json::from_value::<HashMap<String, Value>>(
                        raw_obj.get("metadata").unwrap_or(&Value::Null).to_owned(),
                    )
                    .unwrap_or_default();
                    Some(Datapoint {
                        id,
                        dataset_id,
                        data: data.unwrap().to_owned(),
                        target: raw_obj.get("target").cloned(),
                        metadata,
                    })
                } else {
                    // Otherwise, dump all the fields into the `data` field
                    Some(Datapoint {
                        id,
                        dataset_id,
                        data: raw.to_owned(),
                        target: None,
                        metadata: HashMap::new(),
                    })
                }
            }
            Value::Null => None,
            x => Some(Datapoint {
                id: Uuid::new_v4(),
                dataset_id,
                data: x.to_owned(),
                target: None,
                metadata: HashMap::new(),
            }),
        }
    }

    /// Turns a datapoint into protobuf datapoint for indexing in semantic search service
    ///
    /// Assumes column_name is there in `data`, so it unwraps the field
    ///
    /// Data is a `HashMap<String, String>` and cannot have nested values
    pub fn into_vector_db_datapoint(&self, index_column: &String) -> VectorDBDatapoint {
        let data_map =
            serde_json::from_value::<HashMap<String, NodeInput>>(self.data.to_owned()).unwrap();

        let metadata_map = self
            .metadata
            .iter()
            .map(|(k, v)| (k.to_owned(), json_value_to_string(v)))
            .collect::<HashMap<String, String>>();

        let content: String = match data_map.get(index_column).unwrap() {
            NodeInput::ChatMessageList(messages) => merge_chat_messages(messages),
            _ => data_map.get(index_column).unwrap().clone().into(), // just use from already serialized data
        };

        VectorDBDatapoint {
            content,
            datasource_id: self.dataset_id.to_string(),
            data: metadata_map,
            id: self.id.to_string(),
        }
    }
}

impl From<DBDatapoint> for Datapoint {
    fn from(db_datapoint: DBDatapoint) -> Self {
        Datapoint {
            id: db_datapoint.id,
            dataset_id: db_datapoint.dataset_id,
            data: db_datapoint.data,
            target: db_datapoint.target,
            metadata: serde_json::from_value(db_datapoint.metadata).unwrap_or_default(),
        }
    }
}

pub fn read_bytes_jsonl(bytes: &Vec<u8>) -> Result<Vec<Value>> {
    let buf = BufReader::new(Cursor::new(bytes.as_slice()));
    let reader = serde_jsonlines::JsonLinesReader::new(buf);

    reader
        .read_all::<Value>()
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| anyhow::anyhow!("error parsing jsonlines: {}", e))
}

pub fn read_bytes_json(bytes: &Vec<u8>) -> Result<Vec<Value>> {
    let content = serde_json::from_slice::<Value>(bytes.as_slice())?;
    match content {
        Value::Array(values) => Ok(values),
        _ => Err(anyhow::anyhow!(
            "the file must contain an array of json objects"
        )),
    }
}

pub fn read_bytes_csv(bytes: &Vec<u8>) -> Result<Vec<Value>> {
    let mut reader = csv::Reader::from_reader(bytes.as_slice());
    let headers = reader.headers()?.clone();
    let mut result = Vec::new();
    for record in reader.records() {
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                log::error!("couldn't read line in CSV, {}", e);
                continue;
            }
        };
        let mut row = HashMap::new();
        for i in 0..headers.len() {
            let header = headers
                .get(i)
                .ok_or(anyhow::anyhow!("can't read header at position {}", i))?;
            let value = record.get(i).unwrap_or_default();
            row.insert(header.to_string(), value.to_string());
        }
        let row_json = match serde_json::to_value(row) {
            Ok(v) => v,
            Err(e) => {
                log::error!("couldn't convert csv row to serde_json::Value, {}", e);
                continue;
            }
        };
        result.push(row_json);
    }

    Ok(result)
}

pub async fn insert_datapoints_from_file(
    file_bytes: &Vec<u8>,
    filename: &String,
    dataset_id: Uuid,
    db: Arc<DB>,
) -> Result<Vec<Datapoint>> {
    let mut records = None;
    let extension = filename.split(".").last().unwrap_or_default();
    if extension == "jsonl" {
        records = Some(read_bytes_jsonl(&file_bytes)?);
    } else if extension == "json" {
        records = Some(read_bytes_json(&file_bytes)?);
    } else if extension == "csv" {
        records = Some(read_bytes_csv(&file_bytes)?);
    }

    if let Some(data) = records {
        let datapoints = db::datapoints::insert_raw_data(&db.pool, &dataset_id, &data).await?;
        Ok(datapoints.into_iter().map(|dp| dp.into()).collect())
    } else {
        Err(anyhow::anyhow!(
            "Attempting to process file as unstructured even though requested as structured"
        ))
    }
}
