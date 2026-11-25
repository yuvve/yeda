//! The key and command tables behind the `?` list and `KEYS.md`. `build.rs` includes this file,
//! so it uses nothing from the rest of the crate.

pub type Bindings = Vec<(String, String)>;

/// The actions keys are bound to, also run as `:<name>`.
pub const ACTIONS: &[(&str, &str)] = &[
    ("down", "move down"),
    ("up", "move up"),
    ("half-down", "move half a page down"),
    ("half-up", "move half a page up"),
    ("top", "go to the first entry"),
    ("bottom", "go to the last entry"),
    ("left", "focus the column to the left"),
    ("right", "focus the column to the right"),
    ("next-column", "focus the next column"),
    ("confirm", "end a range (keeping its marks), else focus the column to the right"),
    ("column", "show only the current column; again to show all"),
    ("search", "search; in the library column, filter it (:help explains the syntax)"),
    ("filter", "filter by field, with suggestions from the library"),
    ("sort", "sort"),
    ("clear", "cancel a range, else unmark all, else clear this column's search, else clear every search"),
    ("add", "add by DOI, arXiv ID or ISBN, into a collection you pick; a pasted list adds each"),
    ("open", "open the PDF in the reader (yeda read)"),
    ("edit", "edit the reference file in $EDITOR; on a saved search in the library column, its name, query and sort"),
    ("note", "edit the reference's note in $EDITOR"),
    ("duplicate", "a new reference filled in from the selected one, with its tags, collections and status; on a saved search in the library column, a new search; saved on Enter"),
    ("mark", "mark or unmark the reference"),
    ("range", "mark a range as you move"),
    ("tags", "edit tags"),
    ("move", "move to a collection"),
    ("status-next", "next reading status"),
    ("status-prev", "previous reading status"),
    ("undo", "undo the last command's changes to the library"),
    ("redo", "redo what undo took back"),
    ("annotations", "show annotations"),
    ("extract", "extract the annotations embedded in the PDF"),
    ("tag-color", "colour the selected tag (library column)"),
    ("delete", "delete the reference or saved search"),
    ("command", "command line; Up/Down recall earlier commands, Tab/shift-Tab complete"),
    ("close-or-quit", "close the open table cell, table, graph or annotations, else quit"),
    ("graph-go", "go to the picked neighbour"),
    ("graph-back", "go back"),
    ("graph-tab", "next list under the graph: seeds, cites, cited by, top cited, top citing"),
    ("graph-tab-back", "previous list under the graph"),
    ("graph-sort", "sort the lists by global citations or by year"),
    ("graph-add", "add the picked paper by DOI"),
    ("graph-open", "open the picked paper's DOI in the browser"),
    ("neighbourhood", "show only the selected paper and its direct links"),
    ("graph-scroll-down", "scroll the graph down"),
    ("graph-scroll-up", "scroll the graph up"),
    ("open-annotation", "open the image in the viewer, or the entry in $EDITOR"),
    ("cluster-go", "select the picked paper or label"),
    ("cluster-back", "go back"),
    ("cluster-tab", "list the selected node's links, or every node"),
    ("table-left", "previous column of the table; from the first, the reference list"),
    ("table-right", "next column of the table"),
    ("table-hide", "hide the column"),
    ("table-show", "show the hidden columns again"),
    ("table-cell", "show the cell in full, or close it"),
    ("table-edit", "edit the cell in $EDITOR"),
    ("table-sort", "sort by the column; again reverses"),
    ("table-search", "show only rows with every word in some cell"),
    ("table-filter", "show only rows whose cell in this column matches (!x: doesn't; \"\": empty)"),
    ("table-clear", "close the cell, else clear the table's search and filters, else clear every search, else close the table"),
];

pub const COMMANDS: &[(&str, &str)] = &[
    (":add [--to <collection>] [DOI, arXiv ID or ISBN …]", "add these, or the ones typed in a prompt; with --to, into that collection without asking, and references already in the library join it too"),
    (":do [--llm <profile>] <request>", "have the LLM turn a request into commands, such as add, fetch and summarize; you approve the plan before it runs, and undo takes it all back"),
    (":search <query>", "show only the references the query matches, in the library column; the same as typing it after the search key"),
    (":save <name>", "save the current search"),
    (":summarize[!] [table] [--llm <profile>]", "fill the reference's table row with an LLM; ! overwrites"),
    (":cancel", "stop summarizing after the current paper"),
    (":export <table> <file.csv>", "export a table"),
    (":attach <file.pdf> [location]", "add a PDF, copying or uploading it to a location"),
    (":new", "add a reference by hand, into the selected collection; saved on Enter"),
    (":cache", "download the selected or marked references' remote PDFs that are missing here or changed on the server"),
    (":storage migrate <location>", "copy the PDFs in the default location there, keeping their annotations, then make it the default; the originals stay"),
    (":detach", "remove one of the selected reference's PDFs with its annotations; the file stays where it is"),
    (":sync", "commit, pull and push the library with git"),
    (":diff", "the library's changes since the last :sync; v marks, d d undoes the selected or marked files"),
    (":git-init [url]", "make the library folder a git repository and commit it; with a URL, push it there as origin"),
    (":graph[!] [citations]", "citation graph of the selected or marked references, from OpenAlex (:help explains it)"),
    (":graph tags", "the selected or marked references and their tags, clustered"),
    (":graph col <column>", "the same with a table column's values, such as :graph col group"),
    (":discover[!] [--like] [--llm <profile>] [what]", "ask the LLM for papers like the marked ones (--like), or as described, searching the web and arXiv; with neither it opens a form; ! skips cached results; v marks, a adds"),
    (":found", "the papers the last :discover found"),
    (":history", "past commands; Enter puts one on the command line (Ctrl-R there)"),
    (":setup", "list everything :setup sets, as it is set now; Enter changes one"),
    (":setup provider|llm|storage [name]", "add or change a provider, an LLM profile or a storage location (local, WebDAV, Dropbox) in yeda.toml, testing it first"),
    (":setup openalex", "set the optional OpenAlex API key, testing it first"),
    (":setup web-search [keenable|exa|parallel|firecrawl|searxng]", "the search providers :discover uses, or one provider's optional key or SearXNG URL, testing it first"),
    (":setup proxy", "set the library proxies :fetch opens papers through"),
    (":setup table [name]", "add or change a literature table: its columns and what each holds"),
    (":config [library|device]", "edit yeda.toml, or this device's config.toml, in $EDITOR"),
    (":secrets", "every password, API key and OAuth token the config uses: where each is set; set, replace or remove them"),
    (":envvars", "the environment variable that sets each secret, and which ones need an alias to tell them apart"),
    (":graph close", "close the graph shown"),
    (":table [name]", "a table of the selected or marked references; without a name, pick one (:help explains it)"),
    (":table close", "close the table"),
    (":table rename <old> <new>", "rename a table in yeda.toml and in every paper's LLM file"),
    (":cite [format]", "copy the selected or marked references: bibtex, ident (DOI, arXiv ID, ISBN or URL), a style (ieee, apa, …) or a .csl file"),
    (":fetch [location]", "find and attach PDFs for the selected or marked references (:help explains it)"),
    (":keys", "this list"),
    (":help", "guide: search syntax (AND, OR, NOT, fields), filters, saved searches"),
    (":failed", "what the last add or fetch could not get, with the reasons"),
    (":log", "every typed command, status message and failure, with times, in $EDITOR"),
    (":q", "quit"),
    (":<action>", "any action above, such as :annotations"),
];

/// Editing keys of the command line and every other text box; fixed, not in the keymap.
pub const TEXT_KEYS: &[(&str, &str)] = &[
    ("Left Right Home End", "move the cursor (Ctrl-A Ctrl-E: start / end)"),
    ("Backspace Delete", "delete before / at the cursor"),
    ("Ctrl-W Ctrl-U", "delete the word / everything before the cursor"),
    ("Ctrl-G", "edit the text in $EDITOR (on the command line also Ctrl-X Ctrl-E)"),
    ("Tab Shift-Tab", "on the command line: complete the last word"),
    ("Up Down", "on the command line: earlier / later commands"),
    ("Ctrl-R", "on the command line: past commands containing what is typed (:history)"),
];

pub const NOTES: &[(&str, &str)] = &[
    ("mouse", "a click selects and focuses; the wheel moves the selection, or scrolls the graph"),
    ("narrow", "below 120 columns the right column is hidden, below 80 the library column too"),
    ("--name value", "an option, anywhere in a command (also --name=value; put a value with spaces in \"double quotes\"); -- ends options, so the rest is text"),
    ("keys", "change them under [keys.<context>] in ~/.config/yeda/config.toml (:help shows how)"),
];

/// The description of a bound command line, from the action or command it names.
pub fn describe(command: &str) -> String {
    let name = command.split_whitespace().next().unwrap_or_default();
    let found = ACTIONS.iter().find(|(a, _)| *a == name).map(|(_, d)| *d);
    let found = found.or_else(|| COMMANDS.iter().find(|(c, _)| c.trim_start_matches(':').split([' ', '[', '!']).next() == Some(name)).map(|(_, d)| *d));
    found.map_or_else(|| format!(":{command}"), |d| if command.contains(' ') { format!("{d} (:{command})") } else { d.to_string() })
}

/// The keymap grouped by command per context, then the fixed keys and the commands. A heading
/// row has a key starting with `[`; a blank row separates sections. `contexts` holds each
/// context's bindings as (keys, command line) in definition order.
pub fn rows(contexts: &[(&str, Bindings)]) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for (context, bindings) in contexts {
        let mut grouped: Vec<(&str, Vec<&str>)> = Vec::new();
        for (keys, command) in bindings {
            match grouped.iter_mut().find(|(c, _)| c == command) {
                Some((_, all)) => all.push(keys),
                None => grouped.push((command, vec![keys])),
            }
        }
        if grouped.is_empty() {
            continue;
        }
        rows.push((String::new(), String::new()));
        rows.push((format!("[{context}]"), if *context == "normal" { "everywhere".into() } else { format!("in the {context} column") }));
        rows.extend(grouped.into_iter().map(|(command, keys)| (keys.join(" "), describe(command))));
    }
    rows.push((String::new(), String::new()));
    rows.push(("[text boxes]".into(), "the command line, searches, forms and pickers".into()));
    rows.extend(TEXT_KEYS.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    rows.push((String::new(), String::new()));
    rows.extend(COMMANDS.iter().chain(NOTES).map(|(k, v)| (k.to_string(), v.to_string())));
    rows.remove(0);
    rows
}

/// `rows` as Markdown: a table per section.
#[allow(dead_code)] // only `build.rs` calls it
pub fn markdown(rows: &[(String, String)]) -> String {
    let mut out = String::from("# Keys and commands\n\nGenerated from the built-in key bindings; do not edit.\n");
    let mut open = false;
    for (key, text) in rows {
        if key.is_empty() && text.is_empty() {
            open = false;
        } else if key.starts_with('[') {
            out += &format!("\n## {}\n\n{text}\n", key.trim_matches(['[', ']']));
            out += "\n| Keys | Does |\n| --- | --- |\n";
            open = true;
        } else {
            if !open {
                out += "\n## Commands\n\n| | |\n| --- | --- |\n";
                open = true;
            }
            out += &format!("| `{}` | {} |\n", key.replace('|', "\\|"), text.replace('|', "\\|"));
        }
    }
    out
}
