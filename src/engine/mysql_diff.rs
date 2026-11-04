//! Offline inspection of the standard SQL emitted by dbsnap's dump clients.
//! This is deliberately not a general SQL interpreter. No SQL is executed.
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::io::BufRead;

type Tables = BTreeMap<String, (String, u64)>;

fn identifier(input: &str) -> Result<(String, &str)> {
    let rest = input
        .strip_prefix('`')
        .context("expected quoted dump identifier")?;
    let mut name = String::new();
    let mut chars = rest.char_indices().peekable();
    while let Some((offset, ch)) = chars.next() {
        if ch == '`' {
            if chars.peek().is_some_and(|(_, ch)| *ch == '`') {
                chars.next();
                name.push('`');
            } else {
                return Ok((name, &rest[offset + 1..]));
            }
        } else {
            name.push(ch);
        }
    }
    bail!("unterminated dump identifier")
}

fn tuples(sql: &str) -> Result<u64> {
    let mut bytes = sql.trim().bytes().peekable();
    let mut quote = None;
    let mut depth = 0u64;
    let mut count = 0;
    let mut ended = false;
    while let Some(b) = bytes.next() {
        if ended {
            if !b.is_ascii_whitespace() {
                bail!("unexpected SQL after INSERT");
            }
            continue;
        }
        if let Some(q) = quote {
            if b == b'\\' {
                bytes.next().context("truncated dump escape")?;
            } else if b == q {
                if bytes.peek() == Some(&q) {
                    bytes.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match b {
            b'\'' | b'"' if depth > 0 => quote = Some(b),
            b'(' => {
                if depth == 0 {
                    count += 1;
                }
                depth += 1;
            }
            b')' => depth = depth.checked_sub(1).context("unbalanced INSERT tuple")?,
            b';' if depth == 0 => ended = true,
            b',' | b' ' | b'\t' | b'\r' | b'\n' if depth == 0 => {}
            _ if depth > 0 => {}
            _ => bail!("unsupported dump INSERT syntax"),
        }
    }
    if quote.is_some() || depth != 0 || !ended || count == 0 {
        bail!("incomplete dump INSERT");
    }
    Ok(count)
}

fn inspect(reader: impl BufRead) -> Result<Tables> {
    let mut tables = Tables::new();
    let mut building: Option<(String, String)> = None;
    let mut pending_insert = String::new();
    let mut insert_table = String::new();
    let mut view_placeholder = false;
    let mut stored_code = false;
    for line in reader.lines() {
        let line = line?;
        if !insert_table.is_empty() {
            pending_insert.push('\n');
            pending_insert.push_str(&line);
            if line.ends_with(';') {
                tables
                    .get_mut(&insert_table)
                    .context("INSERT table missing")?
                    .1 += tuples(&pending_insert)?;
                pending_insert.clear();
                insert_table.clear();
            }
            continue;
        }
        if line.starts_with("DELIMITER ") {
            stored_code = line.trim() != "DELIMITER ;";
            continue;
        }
        if stored_code {
            continue;
        }
        if line.starts_with("-- Temporary view structure for view ") {
            view_placeholder = true;
        } else if line.starts_with("-- Table structure for table ")
            || line.starts_with("-- Final view structure for view ")
        {
            view_placeholder = false;
        }
        if view_placeholder {
            continue;
        }
        if let Some((name, definition)) = &mut building {
            // AUTO_INCREMENT is a data-dependent counter, not a schema change.
            let mut normalized = line.clone();
            if let Some(start) = normalized
                .find(" AUTO_INCREMENT=")
                .filter(|_| line.starts_with(')'))
            {
                let end = start + " AUTO_INCREMENT=".len();
                let digits = normalized[end..]
                    .bytes()
                    .take_while(u8::is_ascii_digit)
                    .count();
                normalized.replace_range(start..end + digits, "");
            }
            definition.push_str(&normalized);
            definition.push('\n');
            if line.starts_with(')') && line.ends_with(';') {
                if tables
                    .insert(name.clone(), (definition.clone(), 0))
                    .is_some()
                {
                    bail!("duplicate CREATE TABLE in dump");
                }
                building = None;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("CREATE TABLE ") {
            let (name, tail) = identifier(rest)?;
            if tail.trim() != "(" {
                bail!("unsupported CREATE TABLE dump syntax");
            }
            building = Some((name, String::new()));
        } else if let Some(rest) = line.strip_prefix("INSERT INTO ") {
            let (name, tail) = identifier(rest)?;
            let values = tail
                .strip_prefix(" VALUES")
                .context("unsupported dump INSERT (expected VALUES)")?;
            if line.ends_with(';') {
                tables
                    .get_mut(&name)
                    .context("INSERT without CREATE TABLE")?
                    .1 += tuples(values)?;
            } else {
                insert_table = name;
                pending_insert = values.to_string();
            }
        } else if line.starts_with("REPLACE INTO ") || line.starts_with("LOAD DATA ") {
            bail!("unsupported dump data syntax");
        }
    }
    if building.is_some() || !insert_table.is_empty() {
        bail!("incomplete SQL dump");
    }
    Ok(tables)
}

pub(super) fn compare(left: impl BufRead, right: impl BufRead) -> Result<super::SchemaDiff> {
    let a = inspect(left)?;
    let b = inspect(right)?;
    let mut changes = Vec::new();
    let mut removed_tables = Vec::new();
    for (name, old) in &a {
        match b.get(name) {
            None => {
                removed_tables.push(name.clone());
                changes.push(format!("removed table {name}"));
            }
            Some(new) => {
                if old.0 != new.0 {
                    changes.push(format!("changed schema table {name}"));
                }
                if old.1 != new.1 {
                    changes.push(format!("table {name} rows: {} -> {}", old.1, new.1));
                }
            }
        }
    }
    for name in b.keys() {
        if !a.contains_key(name) {
            changes.push(format!("added table {name}"));
        }
    }
    if changes.is_empty() {
        changes.push("no table schema or row-count differences".into());
    }
    Ok(super::SchemaDiff {
        text: format!("{}\n(row contents, views, routines, triggers and events are not compared; standard dbsnap dump format only)", changes.join("\n")),
        removed_tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_quoted_values_and_nested_parentheses() {
        assert_eq!(
            tuples("(1,'a),(b'),(2,'it\\'s'),(3,'a''b'),(4,POINT(1,2));").unwrap(),
            4
        );
        assert!(tuples("(1,'truncated);").is_err());
        assert!(tuples("(1); DROP TABLE t;").is_err());
    }
    #[test]
    fn compares_empty_tables_schema_and_counts() {
        let a = "CREATE TABLE `a``b` (\n id int\n) ENGINE=InnoDB AUTO_INCREMENT=2;\nINSERT INTO `a``b` VALUES (1);\n";
        let b = "CREATE TABLE `a``b` (\n id int\n) ENGINE=InnoDB AUTO_INCREMENT=3;\nINSERT INTO `a``b` VALUES (1),(2);\n";
        let result = compare(a.as_bytes(), b.as_bytes()).unwrap();
        assert!(result.text.contains("rows: 1 -> 2"));
        assert!(!result.text.contains("changed schema"));
    }

    #[test]
    fn ignores_view_placeholders_and_stored_program_bodies() {
        let sql = "-- Temporary view structure for view `v`\nCREATE TABLE `v` (\n id int\n);\n-- Table structure for table `items`\nCREATE TABLE `items` (\n id int\n);\nDELIMITER ;;\nCREATE PROCEDURE p()\nINSERT INTO `items` VALUES (99);\nDELIMITER ;\nINSERT INTO `items` VALUES (1);\n";
        let tables = inspect(sql.as_bytes()).unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables["items"].1, 1);
    }

    #[test]
    fn rejects_unsupported_or_truncated_data_instead_of_guessing() {
        let header = "CREATE TABLE `items` (\n id int\n);\n";
        for data in [
            "INSERT INTO `items` (id) VALUES (1);",
            "INSERT INTO `items` VALUES (1",
            "REPLACE INTO `items` VALUES (1);",
            "LOAD DATA INFILE 'x' INTO TABLE items;",
        ] {
            assert!(
                inspect(format!("{header}{data}").as_bytes()).is_err(),
                "{data}"
            );
        }
        let tables =
            inspect(format!("{header}INSERT INTO `items` VALUES (1),\n(2);\n").as_bytes()).unwrap();
        assert_eq!(tables["items"].1, 2);
        let maria = inspect(format!("{header}INSERT INTO `items` VALUES\n(1),\n(2);\n").as_bytes())
            .unwrap();
        assert_eq!(maria["items"].1, 2);
    }
}
