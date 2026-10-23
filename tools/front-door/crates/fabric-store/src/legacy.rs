//! Source-shaped legacy terminal rows. Missing and JSON null stay distinct.
use crate::*;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub enum LegacyField {
    #[default]
    Missing,
    Value(Value),
}
impl<'de> Deserialize<'de> for LegacyField {
    fn deserialize<D: serde::Deserializer<'de>>(d:D)->std::result::Result<Self,D::Error> {
        Value::deserialize(d).map(Self::Value)
    }
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TerminalRow {
    #[serde(default)] pub run_id: LegacyField,
    #[serde(default)] pub task_id: LegacyField,
    #[serde(default)] pub attempt: LegacyField,
    #[serde(default)] pub attempt_count: LegacyField,
    #[serde(default)] pub run_dir: LegacyField,
}
impl TerminalRow {
    pub fn from_json(text:&str)->Result<Self> {
        Ok(serde_json::from_value(provenant_records::checked_json::from_str(text)?)?)
    }
}
/// Legacy runs without a nonempty string task ID use String(run_id).
pub fn lane_task_id(row:&TerminalRow)->Result<String> {
    if let LegacyField::Value(Value::String(task))=&row.task_id {
        if !task.is_empty() {return Ok(task.clone());}
    }
    js_string(&row.run_id)
}
impl Store {
    /// Preserve the dynamic source boundary, including nullish attempt fallback.
    pub fn acknowledge_terminal_row(&self,who:&Identity,row:&TerminalRow,busy_timeout_ms:u64)->Result<()> {
        let task=lane_task_id(row)?;
        let run=js_string(&row.run_id)?;
        let attempt=js_number_value(match &row.attempt {
            LegacyField::Missing|LegacyField::Value(Value::Null)=>&row.attempt_count,
            value=>value,
        })?;
        if !(1..=5000).contains(&busy_timeout_ms) {return Err(Error::NoticeTimeout);}
        // better-sqlite3 binds undefined/NaN as NULL, and booleans/objects/arrays
        // are binding errors. Preserve conversion before changing the timeout.
        self.db.busy_timeout(Duration::from_millis(busy_timeout_ms))?;
        let result=self.transaction(true,||{
            let attempt=if attempt.is_nan() {None} else {Some(attempt)};
            self.db.execute("INSERT INTO run_observations(project,recipient_id,run_id,task_id,attempt) VALUES (?,?,?,?,?) ON CONFLICT(project,recipient_id,run_id,task_id) DO UPDATE SET attempt=max(attempt,excluded.attempt)",params![who.project,who.agent_id,run,task,attempt])?;
            let run_dir=sqlite_field(&row.run_dir)?;
            let prefix=format!("{run}:{task}:");let length=prefix.encode_utf16().count() as i64;
            self.db.execute("UPDATE deliveries SET read_at = ? WHERE project = ? AND recipient_id = ? AND read_at IS NULL AND message_id IN (SELECT message_id FROM messages WHERE project = ? AND kind = 'run_terminal' AND ((substr(output_path,1,?) = ? AND CAST(substr(output_path,?) AS INTEGER) <= ?) OR output_path = ?))",params![self.now(),who.project,who.agent_id,who.project,length,prefix,length+1,attempt,run_dir])?;
            Ok(())
        });
        self.restore_default_busy_timeout()?;result
    }
}
fn sqlite_field(field:&LegacyField)->Result<rusqlite::types::Value> {
    use rusqlite::types::Value as Sql;
    Ok(match field {
        LegacyField::Missing|LegacyField::Value(Value::Null)=>Sql::Null,
        LegacyField::Value(Value::String(s))=>Sql::Text(s.clone()),
        LegacyField::Value(Value::Number(n))=>Sql::Real(n.as_f64().ok_or(Error::NumberOutOfRange)?),
        _=>return Err(Error::SqliteBindingType),
    })
}
fn js_number_value(field:&LegacyField)->Result<f64> {
    Ok(match field {
        LegacyField::Missing=>f64::NAN,
        LegacyField::Value(value)=>match value {
            Value::Null=>0.0,Value::Bool(b)=>if *b {1.0}else{0.0},
            Value::Number(n)=>n.as_f64().ok_or(Error::NumberOutOfRange)?,
            _=>pending::js_number(&js_string(field)?).unwrap_or(f64::NAN),
        }
    })
}
fn js_string(field:&LegacyField)->Result<String> {
    Ok(match field {
        LegacyField::Missing=>"undefined".into(),
        LegacyField::Value(value)=>match value {
            Value::Null=>"null".into(),Value::Bool(b)=>b.to_string(),Value::String(s)=>s.clone(),
            Value::Number(n)=>pending::js_number_string(n.as_f64().ok_or(Error::NumberOutOfRange)?),
            Value::Object(object)=>{
                if object.contains_key("toString") {return Err(Error::PrimitiveConversion);}
                "[object Object]".into()
            },
            Value::Array(values)=>values.iter().map(|v|match v {
                Value::Null=>Ok(String::new()),
                v=>js_string(&LegacyField::Value(v.clone())),
            }).collect::<Result<Vec<_>>>()?.join(","),
        }
    })
}
