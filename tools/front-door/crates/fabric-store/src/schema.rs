provenant_sqlite::Schema {
    sql: include_str!("schema.sql"),
    version: 19,
    steps: &[],
    migrations: &[provenant_sqlite::Migration { from: 18, to: 19, sql: include_str!("migration_18_19.sql") }],
}
