//! Query constructors

use crate::{errors::PgmqError, util::CheckedName};
use sqlx::types::chrono::Utc;
pub const TABLE_PREFIX: &str = r#"pgmq"#;
pub const PGMQ_SCHEMA: &str = "public";

pub fn init_queue(name: &str) -> Result<Vec<String>, PgmqError> {
    let name = CheckedName::new(name)?;
    Ok(vec![
        create_meta(),
        create_queue(name)?,
        create_index(name)?,
        create_archive(name)?,
        create_archive_index(name)?,
        insert_meta(name)?,
        grant_pgmon_meta(),
        grant_pgmon_queue(name)?,
    ])
}

pub fn destroy_queue(name: &str) -> Result<Vec<String>, PgmqError> {
    let name = CheckedName::new(name)?;
    Ok(vec![
        drop_queue(name)?,
        delete_queue_index(name)?,
        drop_queue_archive(name)?,
        delete_queue_metadata(name)?,
    ])
}

pub fn create_queue(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        CREATE TABLE IF NOT EXISTS {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name} (
            msg_id BIGSERIAL NOT NULL,
            read_ct INT DEFAULT 0 NOT NULL,
            enqueued_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL,
            vt TIMESTAMP WITH TIME ZONE NOT NULL,
            message JSONB
        );
        "
    ))
}

pub fn create_archive(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        CREATE TABLE IF NOT EXISTS {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}_archive (
            msg_id BIGSERIAL NOT NULL,
            read_ct INT DEFAULT 0 NOT NULL,
            enqueued_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL,
            deleted_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL,
            vt TIMESTAMP WITH TIME ZONE NOT NULL,
            message JSONB
        );
        "
    ))
}

pub fn create_meta() -> String {
    format!(
        "
        CREATE TABLE IF NOT EXISTS {PGMQ_SCHEMA}.{TABLE_PREFIX}_meta (
            queue_name VARCHAR UNIQUE NOT NULL,
            created_at TIMESTAMP WITH TIME ZONE DEFAULT now() NOT NULL
        );
        "
    )
}

fn grant_stmt(table: &str) -> String {
    format!(
        "
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1
    WHERE has_table_privilege('pg_monitor', '{table}', 'SELECT')
  ) THEN
    EXECUTE 'GRANT SELECT ON {table} TO pg_monitor';
  END IF;
END;
$$ LANGUAGE plpgsql;
"
    )
}

// pg_monitor needs to query queue metadata
pub fn grant_pgmon_meta() -> String {
    let table = format!("{PGMQ_SCHEMA}.{TABLE_PREFIX}_meta");
    grant_stmt(&table)
}

// pg_monitor needs to query queue tables
pub fn grant_pgmon_queue(name: CheckedName<'_>) -> Result<String, PgmqError> {
    let table = format!("{PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}");
    Ok(grant_stmt(&table))
}

pub fn grant_pgmon_queue_seq(name: CheckedName<'_>) -> Result<String, PgmqError> {
    let table = format!("{PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}_msg_id_seq");
    Ok(grant_stmt(&table))
}

pub fn drop_queue(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        DROP TABLE IF EXISTS {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name};
        "
    ))
}

pub fn delete_queue_index(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        DROP INDEX IF EXISTS {TABLE_PREFIX}_{name}.vt_idx_{name};
        "
    ))
}

pub fn delete_queue_metadata(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        DO $$
        BEGIN
           IF EXISTS (
                SELECT 1
                FROM information_schema.tables
                WHERE table_name = '{TABLE_PREFIX}_meta')
            THEN
              DELETE
              FROM {PGMQ_SCHEMA}.{TABLE_PREFIX}_meta
              WHERE queue_name = '{name}';
           END IF;
        END $$;
        "
    ))
}

pub fn drop_queue_archive(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        DROP TABLE IF EXISTS {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}_archive;
        "
    ))
}

pub fn insert_meta(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        INSERT INTO {PGMQ_SCHEMA}.{TABLE_PREFIX}_meta (queue_name)
        VALUES ('{name}')
        ON CONFLICT
        DO NOTHING;
        "
    ))
}

pub fn create_archive_index(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        CREATE INDEX IF NOT EXISTS deleted_at_idx_{name} ON {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}_archive (deleted_at);
        "
    ))
}

// indexes are created ascending to support FIFO
pub fn create_index(name: CheckedName<'_>) -> Result<String, PgmqError> {
    Ok(format!(
        "
        CREATE INDEX IF NOT EXISTS msg_id_vt_idx_{name} ON {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name} (vt ASC, msg_id ASC);
        "
    ))
}

pub fn enqueue(
    name: &str,
    messages: &[serde_json::Value],
    delay: &u64,
) -> Result<String, PgmqError> {
    // construct string of comma separated messages
    check_input(name)?;
    let mut values = "".to_owned();
    for message in messages.iter() {
        let full_msg = format!("((now() + interval '{delay} seconds'), '{message}'::json),");
        values.push_str(&full_msg)
    }
    // drop trailing comma from constructed string
    values.pop();
    Ok(format!(
        "
        INSERT INTO {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name} (vt, message)
        VALUES {values}
        RETURNING msg_id;
        "
    ))
}

pub fn read(name: &str, vt: i32, limit: i32) -> Result<String, PgmqError> {
    check_input(name)?;
    Ok(format!(
        "
    WITH cte AS
        (
            SELECT msg_id
            FROM {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
            WHERE vt <= now()
            ORDER BY msg_id ASC
            LIMIT {limit}
            FOR UPDATE SKIP LOCKED
        )
    UPDATE {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
    SET
        vt = now() + interval '{vt} seconds',
        read_ct = read_ct + 1
    WHERE msg_id in (select msg_id from cte)
    RETURNING *;
    "
    ))
}

pub fn delete(name: &str, msg_id: i64) -> Result<String, PgmqError> {
    check_input(name)?;
    Ok(format!(
        "
        DELETE FROM {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
        WHERE msg_id = {msg_id};
        "
    ))
}

pub fn set_vt(name: &str, msg_id: i64, vt: chrono::DateTime<Utc>) -> Result<String, PgmqError> {
    check_input(name)?;
    Ok(format!(
        "
        UPDATE {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
        SET vt = '{t}'::timestamp
        WHERE msg_id = {msg_id}
        RETURNING *;
        ",
        t = vt.format("%Y-%m-%d %H:%M:%S%.3f %z")
    ))
}

pub fn delete_batch(name: &str, msg_ids: &[i64]) -> Result<String, PgmqError> {
    // construct string of comma separated msg_id
    check_input(name)?;
    let mut msg_id_list: String = "".to_owned();
    for msg_id in msg_ids.iter() {
        let id_str = format!("{msg_id},");
        msg_id_list.push_str(&id_str)
    }
    // drop trailing comma from constructed string
    msg_id_list.pop();
    Ok(format!(
        "
        DELETE FROM {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
        WHERE msg_id in ({msg_id_list});
        "
    ))
}

pub fn archive(name: &str, msg_id: i64) -> Result<String, PgmqError> {
    check_input(name)?;
    Ok(format!(
        "
        WITH archived AS (
            DELETE FROM {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
            WHERE msg_id = {msg_id}
            RETURNING msg_id, vt, read_ct, enqueued_at, message
        )
        INSERT INTO {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}_archive (msg_id, vt, read_ct, enqueued_at, message)
        SELECT msg_id, vt, read_ct, enqueued_at, message
        FROM archived;
        "
    ))
}

pub fn pop(name: &str) -> Result<String, PgmqError> {
    check_input(name)?;
    Ok(format!(
        "
        WITH cte AS
            (
                SELECT msg_id
                FROM {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
                WHERE vt <= now()
                ORDER BY msg_id ASC
                LIMIT 1
                FOR UPDATE SKIP LOCKED
            )
        DELETE from {PGMQ_SCHEMA}.{TABLE_PREFIX}_{name}
        WHERE msg_id = (select msg_id from cte)
        RETURNING *;
        "
    ))
}

/// panics if input is invalid. otherwise does nothing.
pub fn check_input(input: &str) -> Result<(), PgmqError> {
    // Docs:
    // https://www.postgresql.org/docs/current/sql-syntax-lexical.html#SQL-SYNTAX-IDENTIFIERS

    // Default value of `NAMEDATALEN`, set in `src/include/pg_config_manual.h`
    const NAMEDATALEN: usize = 64;
    // The maximum length of an identifier.
    // Longer names can be used in commands, but they'll be truncated
    const MAX_IDENTIFIER_LEN: usize = NAMEDATALEN - 1;
    // The max length of a PGMQ table, considering its prefix and the underline after it (e.g. "pgmq_")
    const MAX_PGMQ_TABLE_LEN: usize = MAX_IDENTIFIER_LEN - TABLE_PREFIX.len() - 1;

    let is_short_enough = input.len() <= MAX_PGMQ_TABLE_LEN;
    let has_valid_characters = input
        .as_bytes()
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'_');
    let valid = is_short_enough && has_valid_characters;
    match valid {
        true => Ok(()),
        false => Err(PgmqError::InvalidQueueName {
            name: input.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create() {
        let queue_name = CheckedName::new("yolo").unwrap();
        let query = create_queue(queue_name);
        assert!(query.unwrap().contains("pgmq_yolo"));
    }

    #[test]
    fn test_enqueue() {
        let mut msgs: Vec<serde_json::Value> = Vec::new();
        let msg = serde_json::json!({
            "foo": "bar"
        });
        msgs.push(msg);
        let query = enqueue("yolo", &msgs, &0).unwrap();
        assert!(query.contains("pgmq_yolo"));
        assert!(query.contains("{\"foo\":\"bar\"}"));
    }

    #[test]
    fn test_read() {
        let qname = "myqueue";
        let vt: i32 = 20;
        let limit: i32 = 1;

        let query = read(&qname, vt, limit).unwrap();

        assert!(query.contains(&qname));
        assert!(query.contains(&vt.to_string()));
    }

    #[test]
    fn test_delete() {
        let qname = "myqueue";
        let msg_id: i64 = 42;

        let query = delete(&qname, msg_id).unwrap();

        assert!(query.contains(&qname));
        assert!(query.contains(&msg_id.to_string()));
    }

    #[test]
    fn test_delete_batch() {
        let mut msg_ids: Vec<i64> = Vec::new();
        let qname = "myqueue";
        msg_ids.push(42);
        msg_ids.push(43);
        msg_ids.push(44);

        let query = delete_batch(&qname, &msg_ids).unwrap();

        assert!(query.contains(&qname));
        for id in msg_ids.iter() {
            assert!(query.contains(&id.to_string()));
        }
    }

    #[test]
    fn check_input_rejects_names_too_large() {
        let table_name = "my_valid_table_name";
        assert!(check_input(table_name).is_ok());

        assert!(check_input(&"a".repeat(58)).is_ok());

        assert!(check_input(&"a".repeat(59)).is_err());
        assert!(check_input(&"a".repeat(60)).is_err());
        assert!(check_input(&"a".repeat(70)).is_err());
    }

    #[test]
    fn test_check_input() {
        let invalids = vec!["bad;queue_name", "bad name", "bad--name"];
        for i in invalids.iter() {
            let is_valid = check_input(i);
            assert!(is_valid.is_err())
        }
        let valids = vec!["good_queue", "greatqueue", "my_great_queue"];
        for i in valids.iter() {
            let is_valid = check_input(i);
            assert!(is_valid.is_ok())
        }
    }
}
