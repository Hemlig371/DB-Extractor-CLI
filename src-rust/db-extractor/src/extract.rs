//! Потоковое чтение результата запроса через connectorx.
//!
//! Источник выбирается по схеме URI (`oracle://`, `postgresql://`, `clickhouse://`, ...) во время
//! работы; все источники скомпилированы в бинарник.

use std::convert::TryFrom;

use arrow::datatypes::SchemaRef;
use arrow::record_batch::RecordBatch;
use connectorx::arrow_batch_iter::RecordBatchIterator;
use connectorx::get_arrow::new_record_batch_iter;
use connectorx::partition::{partition, PartitionQuery};
use connectorx::source_router::SourceConn;
use connectorx::sql::CXQuery;

use crate::guard::guard;

pub struct Stream {
    iter: Box<dyn RecordBatchIterator>,
    /// Схема результата из самого источника (до преобразований).
    pub schema: SchemaRef,
}

/// Готовит запрос: получает схему результата, но строки ещё не читает.
///
/// `partition`: (колонка, число частей) для параллельного чтения.
pub fn open(conn: &str, sql: &str, partition_by: Option<(&str, usize)>, batch_size: usize) -> Result<Stream, String> {
    guard(|| -> Result<Stream, String> {
        let source_conn = SourceConn::try_from(conn).map_err(|e| e.to_string())?;

        let (queries, origin_query): (Vec<CXQuery<String>>, Option<String>) = match partition_by {
            Some((column, num)) => {
                // connectorx оборачивает запрос в подзапрос без перевода строки; завершающий
                // `-- комментарий` (например аннотация) иначе «съел» бы закрывающую скобку.
                let query = format!("{}\n", sql);
                let part = PartitionQuery::new(&query, column, None, None, num);
                let queries = partition(&part, &source_conn).map_err(|e| e.to_string())?;
                (queries, Some(query))
            }
            None => (vec![CXQuery::naked(sql)], None),
        };

        let iter =
            new_record_batch_iter(&source_conn, origin_query, &queries, batch_size, None).map_err(|e| e.to_string())?;
        let schema = iter.get_schema().0.schema();
        Ok(Stream { iter, schema })
    })?
}

impl Stream {
    /// Запускает чтение строк в фоновых потоках connectorx.
    pub fn start(&mut self) -> Result<(), String> {
        guard(|| self.iter.prepare())
    }

    /// Следующий пакет строк; `None` когда данные закончились. Сбой источника приходит как `Err`.
    pub fn next_batch(&mut self) -> Result<Option<RecordBatch>, String> {
        guard(|| self.iter.next_batch())
    }
}
