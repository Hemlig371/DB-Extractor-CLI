//! Чистая логика db-extractor без внешних зависимостей: разбор SQL, аннотации `-- @DX_*`,
//! выбор формата и сжатия, сопоставление колонок, маскирование секретов, формат журнала, текст значений
//! для CSV и JSONL.

pub mod annotations;
pub mod columns;
pub mod compression;
pub mod connstr;
pub mod logfmt;
pub mod secrets;
pub mod sqlsplit;
pub mod textfmt;
