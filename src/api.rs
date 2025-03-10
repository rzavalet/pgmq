use pgrx::prelude::*;
use pgrx::spi;
use pgrx::spi::SpiTupleTable;

use crate::partition::PARTMAN_SCHEMA;
use pgmq_crate::query::{PGMQ_SCHEMA, TABLE_PREFIX};

#[pg_extern]
fn pgmq_drop_queue(
    queue_name: String,
    partitioned: default!(bool, false),
) -> Result<bool, spi::Error> {
    delete_queue(queue_name, partitioned)?;
    Ok(true)
}

pub fn delete_queue(queue_name: String, partitioned: bool) -> Result<(), spi::Error> {
    // TODO: we should keep track whether queue is partitioned in pgmq_meta
    // then read that to determine we want to delete the part_config entries
    // this should go out before 1.0
    let queue_table = format!("{PGMQ_SCHEMA}.{TABLE_PREFIX}_{queue_name}");
    let mut queries = vec![
        format!("DELETE from {PGMQ_SCHEMA}.pgmq_meta WHERE queue_name = '{queue_name}';"),
        format!("DROP TABLE {queue_table};"),
    ];
    if partitioned {
        queries.push(format!(
            "DELETE FROM {PARTMAN_SCHEMA}.part_config where parent_table = '{queue_table}';"
        ))
    }
    let _: Result<(), spi::Error> = Spi::connect(|mut client| {
        for q in queries {
            client.update(q.as_str(), None, None)?;
        }
        Ok(())
    });
    Ok(())
}

#[pg_extern]
fn pgmq_list_queues() -> Result<
    TableIterator<
        'static,
        (
            name!(queue_name, String),
            name!(created_at, TimestampWithTimeZone),
        ),
    >,
    spi::Error,
> {
    let results = listit()?;
    Ok(TableIterator::new(results))
}

pub fn listit() -> Result<Vec<(String, TimestampWithTimeZone)>, spi::Error> {
    let mut results: Vec<(String, TimestampWithTimeZone)> = Vec::new();
    let query = format!("SELECT * FROM {PGMQ_SCHEMA}.pgmq_meta");
    let _: Result<(), spi::Error> = Spi::connect(|client| {
        let tup_table: SpiTupleTable = client.select(&query, None, None)?;
        for row in tup_table {
            let queue_name = row["queue_name"].value::<String>()?.expect("no queue_name");
            let created_at = row["created_at"]
                .value::<TimestampWithTimeZone>()?
                .expect("no created_at");
            results.push((queue_name, created_at));
        }
        Ok(())
    });
    Ok(results)
}
