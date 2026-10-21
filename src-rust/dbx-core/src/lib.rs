//! Чистая логика db-extractor без внешних зависимостей: разбор SQL, аннотации `-- @DX_*`,
//! выбор формата и сжатия, сопоставление колонок, маскирование секретов, формат журнала.
//!
//! Всё здесь повторяет поведение Python-версии (`src-python/main.py`) и покрыто теми же
//! тестами, поэтому крейт можно проверять без базы данных и без Arrow.

pub mod annotations;
pub mod columns;
pub mod compression;
pub mod logfmt;
pub mod secrets;
pub mod sqlsplit;
