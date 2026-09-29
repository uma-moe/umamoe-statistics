use anyhow::{anyhow, Context, Result};
use csv::StringRecord;
use std::io::Read;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

use super::*;
use crate::resources::{printProgress, ResourceMonitor};
pub(crate) fn streamRowsSerial<R: Read>(
    csv_reader: &mut csv::Reader<R>,
    headers: &StringRecord,
    compiler: &mut Compiler,
    progress_every: u64,
    support_card_types: &SupportCardTypes,
    resource_monitor: &mut Option<ResourceMonitor>,
    started_at: Instant,
) -> Result<()> {
    for result in csv_reader.records() {
        let record = result.context("read COPY row")?;
        let row = parseRecord(headers, &record)?;
        compiler.addRow(row, support_card_types);

        if progress_every > 0 && compiler.total_entries % progress_every == 0 {
            printProgress(
                resource_monitor,
                "processed",
                compiler.total_entries,
                started_at.elapsed(),
            );
        }
    }

    Ok(())
}

pub(crate) fn streamRowsParallel<R: Read>(
    csv_reader: &mut csv::Reader<R>,
    headers: &StringRecord,
    compiler: &mut Compiler,
    dataset_version: &str,
    worker_threads: usize,
    batch_rows: usize,
    progress_every: u64,
    support_card_types: Arc<SupportCardTypes>,
    resource_monitor: &mut Option<ResourceMonitor>,
    started_at: Instant,
) -> Result<()> {
    let (batch_sender, batch_receiver) = mpsc::sync_channel::<Vec<RowData>>(worker_threads * 2);
    let batch_receiver = Arc::new(Mutex::new(batch_receiver));
    let (result_sender, result_receiver) = mpsc::channel::<Compiler>();
    let mut handles = Vec::with_capacity(worker_threads);

    for _ in 0..worker_threads {
        let batch_receiver = Arc::clone(&batch_receiver);
        let result_sender = result_sender.clone();
        let dataset_version = dataset_version.to_string();
        let support_card_types = Arc::clone(&support_card_types);
        handles.push(thread::spawn(move || loop {
            let batch = {
                let receiver = batch_receiver.lock().expect("batch receiver lock poisoned");
                receiver.recv()
            };

            let Ok(batch) = batch else {
                break;
            };

            let mut partial = Compiler::new(dataset_version.clone());
            for row in batch {
                partial.addRow(row, support_card_types.as_ref());
            }
            partial.finish();

            if result_sender.send(partial).is_err() {
                break;
            }
        }));
    }
    drop(result_sender);

    let mut batch = Vec::with_capacity(batch_rows);
    let mut current_trainer_id: Option<String> = None;
    let mut rows_read = 0_u64;
    let mut sent_batches = 0_usize;
    let mut merged_batches = 0_usize;
    let mut next_progress = progress_every;

    for result in csv_reader.records() {
        let record = result.context("read COPY row")?;
        let row = parseRecord(headers, &record)?;
        let starts_new_trainer = current_trainer_id
            .as_deref()
            .map_or(false, |trainer_id| trainer_id != row.trainer_id);

        if starts_new_trainer && batch.len() >= batch_rows {
            let full_batch = std::mem::replace(&mut batch, Vec::with_capacity(batch_rows));
            batch_sender
                .send(full_batch)
                .map_err(|_| anyhow!("statistics worker stopped before receiving a batch"))?;
            sent_batches += 1;
            mergeAvailableResults(&result_receiver, compiler, &mut merged_batches);
        }

        current_trainer_id = Some(row.trainer_id.clone());
        batch.push(row);
        rows_read += 1;

        if progress_every > 0 && rows_read >= next_progress {
            printProgress(resource_monitor, "queued", rows_read, started_at.elapsed());
            while next_progress <= rows_read {
                next_progress += progress_every;
            }
            mergeAvailableResults(&result_receiver, compiler, &mut merged_batches);
        }
    }

    if !batch.is_empty() {
        batch_sender
            .send(batch)
            .map_err(|_| anyhow!("statistics worker stopped before receiving the final batch"))?;
        sent_batches += 1;
    }
    drop(batch_sender);

    while merged_batches < sent_batches {
        let partial = result_receiver
            .recv()
            .context("receive worker statistics")?;
        compiler.merge(partial);
        merged_batches += 1;
    }

    for handle in handles {
        handle
            .join()
            .map_err(|_| anyhow!("statistics worker panicked"))?;
    }

    Ok(())
}

fn mergeAvailableResults(
    result_receiver: &mpsc::Receiver<Compiler>,
    compiler: &mut Compiler,
    merged_batches: &mut usize,
) {
    while let Ok(partial) = result_receiver.try_recv() {
        compiler.merge(partial);
        *merged_batches += 1;
    }
}

pub(crate) fn statisticsCopyQuery(limit: Option<u64>) -> String {
    let limit_clause = limit.map_or(String::new(), |value| format!(" LIMIT {value}"));
    format!(
        "COPY (\
            SELECT \
                ts.trainer_id::text AS trainer_id, \
                ts.card_id::bigint AS card_id, \
                ts.distance_type::int AS distance_type, \
                COALESCE(ts.scenario_id, 1)::int AS scenario_id, \
                ts.running_style::int AS running_style, \
                ts.speed::int AS speed, \
                ts.power::int AS power, \
                ts.stamina::int AS stamina, \
                ts.wiz::int AS wiz, \
                ts.guts::int AS guts, \
                ts.rank_score::int AS rank_score, \
                COALESCE(ts.skills::text, '[]') AS skills, \
                COALESCE(ts.support_cards::text, '[]') AS support_cards, \
                t.team_class::int AS team_class \
            FROM team_stadium ts \
            JOIN trainer t ON ts.trainer_id = t.account_id \
            WHERE t.team_evaluation_point > 2500 \
            ORDER BY ts.trainer_id, ts.distance_type, ts.member_id\
            {limit_clause}\
        ) TO STDOUT WITH (FORMAT csv, HEADER true)"
    )
}

fn parseRecord(headers: &StringRecord, record: &StringRecord) -> Result<RowData> {
    Ok(RowData {
        trainer_id: field(headers, record, "trainer_id")?.to_string(),
        card_id: parseRequired(headers, record, "card_id")?,
        distance_type: parseRequired(headers, record, "distance_type")?,
        scenario_id: parseRequired(headers, record, "scenario_id")?,
        running_style: parseRequired(headers, record, "running_style")?,
        team_class: parseOptional(headers, record, "team_class")?,
        stats: [
            parseRequired(headers, record, "speed")?,
            parseRequired(headers, record, "power")?,
            parseRequired(headers, record, "stamina")?,
            parseRequired(headers, record, "wiz")?,
            parseRequired(headers, record, "guts")?,
            parseRequired(headers, record, "rank_score")?,
        ],
        skills: parseU32Array(field(headers, record, "skills")?),
        support_cards: parseU32Array(field(headers, record, "support_cards")?),
    })
}

fn parseU32Array(input: &str) -> Vec<u32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if trimmed.starts_with('[') {
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return Vec::new();
        };
        let Some(array) = value.as_array() else {
            return Vec::new();
        };
        return array
            .iter()
            .filter_map(|value| jsonU32(Some(value)))
            .collect();
    }

    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return trimmed[1..trimmed.len() - 1]
            .split(',')
            .filter_map(|value| {
                let value = value.trim().trim_matches('"');
                if value.is_empty() || value.eq_ignore_ascii_case("null") {
                    None
                } else {
                    value.parse::<u32>().ok()
                }
            })
            .collect();
    }

    Vec::new()
}

fn field<'a>(headers: &StringRecord, record: &'a StringRecord, name: &str) -> Result<&'a str> {
    let index = headers
        .iter()
        .position(|header| header == name)
        .ok_or_else(|| anyhow!("missing COPY column {name}"))?;
    record
        .get(index)
        .ok_or_else(|| anyhow!("missing value for {name}"))
}

fn parseRequired<T>(headers: &StringRecord, record: &StringRecord, name: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    field(headers, record, name)?
        .parse::<T>()
        .map_err(|error| anyhow!("invalid {name}: {error}"))
}

fn parseOptional<T>(headers: &StringRecord, record: &StringRecord, name: &str) -> Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let value = field(headers, record, name)?;
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<T>()
            .map(Some)
            .map_err(|error| anyhow!("invalid {name}: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn compileCsv(csv: &[u8], workers: usize) -> Result<Compiler> {
        let support_types = Arc::new(loadSupportCardTypes()?);
        let mut compiler = Compiler::new("regression".to_string());
        compiler.generated_at = "2026-09-25T00:00:00".to_string();
        let mut reader = csv::Reader::from_reader(csv);
        let headers = reader.headers()?.clone();
        if workers == 1 {
            streamRowsSerial(
                &mut reader,
                &headers,
                &mut compiler,
                0,
                &support_types,
                &mut None,
                Instant::now(),
            )?;
        } else {
            streamRowsParallel(
                &mut reader,
                &headers,
                &mut compiler,
                "regression",
                workers,
                2,
                0,
                support_types,
                &mut None,
                Instant::now(),
            )?;
        }
        compiler.finish();
        Ok(compiler)
    }

    fn assertHistogramTotals(value: &Value) {
        if let Some(object) = value.as_object() {
            if let Some(histogram) = object.get("histogram") {
                let total: u64 = histogram
                    .as_object()
                    .unwrap()
                    .values()
                    .map(|count| count.as_u64().unwrap())
                    .sum();
                assert_eq!(json!(total), object["count"]);
            }
            for child in object.values() {
                assertHistogramTotals(child);
            }
        }
    }

    #[test]
    fn serialAndParallelExportsAgreeForNewScenarios() -> Result<()> {
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer.write_record([
            "trainer_id",
            "card_id",
            "distance_type",
            "scenario_id",
            "running_style",
            "team_class",
            "speed",
            "power",
            "stamina",
            "wiz",
            "guts",
            "rank_score",
            "skills",
            "support_cards",
        ])?;
        for trainer in 0..4 {
            for row in 0..121 {
                writer.write_record([
                    trainer.to_string(),
                    (100101 + row % 2 * 100).to_string(),
                    (row % 2 + 1).to_string(),
                    (3 + trainer % 2 * 6).to_string(),
                    "1".to_string(),
                    if trainer == 3 {
                        String::new()
                    } else {
                        "6".to_string()
                    },
                    (1200 + row % 101).to_string(),
                    "1300".to_string(),
                    "1400".to_string(),
                    "1500".to_string(),
                    "1600".to_string(),
                    "23001".to_string(),
                    "[100024,100011]".to_string(),
                    if trainer % 2 == 0 {
                        "[100214,300674]"
                    } else {
                        "[100014,1000142]"
                    }
                    .to_string(),
                ])?;
            }
        }
        let csv = writer.into_inner()?;
        let serial = compileCsv(&csv, 1)?;
        let parallel = compileCsv(&csv, 3)?;
        assert_eq!(serial.indexJson(), parallel.indexJson());
        assert_eq!(serial.globalJson(), parallel.globalJson());
        assert_eq!(serial.total_entries, 484);
        assert_eq!(serial.trainer_counts.total_trainers, 4);
        let global = serial.globalJson();
        assert_eq!(
            global["stat_averages"]["by_scenario"]["9"]["speed"]["max"],
            1300
        );
        assert_eq!(
            global["stat_averages"]["by_scenario"]["3"]["rank_score"]["max"],
            23001
        );
        assertHistogramTotals(&global);
        for (id, character) in &serial.characters {
            let report = serial.characterJson(*id, character);
            assert_eq!(
                report,
                parallel.characterJson(*id, &parallel.characters[id])
            );
            assertHistogramTotals(&report);
        }

        // Exercise staging, gzip payloads, and replacement of an existing dataset.
        let output = std::env::temp_dir().join(format!(
            "umamoe-statistics-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&output)?;
        serial.writeOutputs(&output)?;
        parallel.writeOutputs(&output)?;
        let master: Value = serde_json::from_slice(&std::fs::read(output.join("datasets.json"))?)?;
        assert_eq!(master["datasets"].as_array().unwrap().len(), 1);
        assert_eq!(master["datasets"][0]["index"], serial.indexJson());
        let compressed = std::fs::File::open(output.join("regression/global/global.json.gz"))?;
        let mut stored = Vec::new();
        flate2::read::GzDecoder::new(compressed).read_to_end(&mut stored)?;
        assert_eq!(stored, serde_json::to_vec(&global)?);
        std::fs::remove_dir_all(output)?;
        Ok(())
    }

    #[test]
    #[ignore = "requires an isolated PostgreSQL URL in STATISTICS_TEST_DATABASE_URL"]
    fn copyQueryExcludesIneligibleTrainersBeforeLimiting() -> Result<()> {
        let url = std::env::var("STATISTICS_TEST_DATABASE_URL")?;
        let mut client = postgres::Client::connect(&url, postgres::NoTls)?;
        client.batch_execute(
            "CREATE TEMP TABLE trainer (account_id bigint PRIMARY KEY, team_class int, team_evaluation_point int);
             CREATE TEMP TABLE team_stadium (
                 trainer_id bigint, card_id bigint, distance_type int, member_id int,
                 scenario_id int, running_style int, speed int, power int, stamina int,
                 wiz int, guts int, rank_score int, skills jsonb, support_cards jsonb
             );
             INSERT INTO trainer VALUES (1, 6, 0), (2, 6, 2500), (3, 6, 2501), (4, 6, 9000), (5, 6, NULL), (6, 6, 2499);
             INSERT INTO team_stadium
             SELECT trainer_id, 100101, 1, member_id, 9, 1, 2001, 1900, 1800, 1700, 1600, 30001, '[100024]', '[100214]'
             FROM generate_series(1, 7) trainer_id CROSS JOIN generate_series(1, 2) member_id;
             UPDATE team_stadium SET speed = 9999 WHERE trainer_id NOT IN (3, 4);"
        )?;

        for (limit, expected_rows, expected_trainers) in
            [(None, 4, 2), (Some(1), 1, 1), (Some(0), 0, 0)]
        {
            let mut csv = Vec::new();
            client
                .copy_out(statisticsCopyQuery(limit).as_str())?
                .read_to_end(&mut csv)?;
            for workers in [1, 3] {
                let compiler = compileCsv(&csv, workers)?;
                assert_eq!(compiler.total_entries, expected_rows);
                assert_eq!(compiler.trainer_counts.total_trainers, expected_trainers);
                if expected_rows > 0 {
                    assert_eq!(compiler.global.stats[0].min, Some(2001));
                    assert_eq!(compiler.global.stats[0].max, Some(2001));
                }
                assertHistogramTotals(&compiler.globalJson());
            }
        }
        Ok(())
    }

    #[test]
    fn parsesJsonAndPostgresU32Arrays() {
        assert_eq!(parseU32Array("[100011, \"100024\"]"), vec![100011, 100024]);
        assert_eq!(parseU32Array("{100011,100024,NULL}"), vec![100011, 100024]);
        assert_eq!(parseU32Array("{}"), Vec::<u32>::new());
    }
}
