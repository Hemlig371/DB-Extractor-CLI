//! Чистая логика db-extractor без внешних зависимостей: разбор SQL, аннотации `-- @DX_*`,
//! выбор формата и сжатия, сопоставление колонок, маскирование секретов, формат журнала.

pub mod annotations;
pub mod columns;
pub mod compression;
pub mod logfmt;
pub mod secrets;
pub mod sqlsplit;
