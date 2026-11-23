# Keys and commands

Generated from the built-in key bindings; do not edit.

## normal

everywhere

| Keys | Does |
| --- | --- |
| `j Down` | move down |
| `k Up` | move up |
| `ctrl-d` | move half a page down |
| `ctrl-u` | move half a page up |
| `gg` | go to the first entry |
| `G` | go to the last entry |
| `h Left` | focus the column to the left |
| `l Right` | focus the column to the right |
| `Enter` | end a range (keeping its marks), else focus the column to the right |
| `Tab` | focus the next column |
| `c` | show only the current column; again to show all |
| `/` | search; in the library column, filter it (:help explains the syntax) |
| `f` | filter by field, with suggestions from the library |
| `s` | sort |
| `Esc` | cancel a range, else unmark all, else clear this column's search, else clear every search |
| `a` | add by DOI, arXiv ID or ISBN, into a collection you pick; a pasted list adds each |
| `o` | open the PDF in the reader (yeda read) |
| `e` | edit the reference file in $EDITOR; on a saved search in the library column, its name, query and sort |
| `n` | edit the reference's note in $EDITOR |
| `v` | mark or unmark the reference |
| `V` | mark a range as you move |
| `t` | edit tags |
| `m` | move to a collection |
| `r` | next reading status |
| `R` | previous reading status |
| `u` | undo the last command's changes to the library |
| `ctrl-r` | redo what undo took back |
| `A` | show annotations |
| `T` | a table of the selected or marked references; without a name, pick one (:help explains it) |
| `x` | extract the annotations embedded in the PDF |
| `C` | colour the selected tag (library column) |
| `dd` | delete the reference or saved search |
| `yy` | a new reference filled in from the selected one, with its tags, collections and status; on a saved search in the library column, a new search; saved on Enter |
| `:` | command line; Up/Down recall earlier commands, Tab/shift-Tab complete |
| `?` | this list |
| `q` | close the open table cell, table, graph or annotations, else quit |
| `ctrl-e` | scroll the graph down |
| `ctrl-y` | scroll the graph up |

## graph

in the graph column

| Keys | Does |
| --- | --- |
| `Enter` | go to the picked neighbour |
| `Backspace` | go back |
| `Tab` | next list under the graph: seeds, cites, cited by, top cited, top citing |
| `shift-Tab` | previous list under the graph |
| `s` | sort the lists by global citations or by year |
| `N` | show only the selected paper and its direct links |
| `a` | add the picked paper by DOI |
| `o` | open the picked paper's DOI in the browser |
| `Esc` | citation graph of the selected or marked references, from OpenAlex (:help explains it) (:graph close) |

## cluster

in the cluster column

| Keys | Does |
| --- | --- |
| `Enter` | select the picked paper or label |
| `Backspace` | go back |
| `Tab shift-Tab` | list the selected node's links, or every node |
| `Esc` | citation graph of the selected or marked references, from OpenAlex (:help explains it) (:graph close) |

## table

in the table column

| Keys | Does |
| --- | --- |
| `h Left` | previous column of the table; from the first, the reference list |
| `l Right` | next column of the table |
| `-` | hide the column |
| `+` | show the hidden columns again |
| `Enter` | show the cell in full, or close it |
| `e` | edit the cell in $EDITOR |
| `s` | sort by the column; again reverses |
| `/` | show only rows with every word in some cell |
| `f` | show only rows whose cell in this column matches (!x: doesn't; "": empty) |
| `Esc` | close the cell, else clear the table's search and filters, else clear every search, else close the table |

## annotations

in the annotations column

| Keys | Does |
| --- | --- |
| `o` | open the image in the viewer, or the entry in $EDITOR |

## text boxes

the command line, searches, forms and pickers

| Keys | Does |
| --- | --- |
| `Left Right Home End` | move the cursor (Ctrl-A Ctrl-E: start / end) |
| `Backspace Delete` | delete before / at the cursor |
| `Ctrl-W Ctrl-U` | delete the word / everything before the cursor |
| `Ctrl-G` | edit the text in $EDITOR (on the command line also Ctrl-X Ctrl-E) |
| `Tab Shift-Tab` | on the command line: complete the last word |
| `Up Down` | on the command line: earlier / later commands |
| `Ctrl-R` | on the command line: past commands containing what is typed (:history) |

## Commands

| | |
| --- | --- |
| `:add [--to <collection>] [DOI, arXiv ID or ISBN …]` | add these, or the ones typed in a prompt; with --to, into that collection without asking, and references already in the library join it too |
| `:do [--llm <profile>] <request>` | have the LLM turn a request into commands, such as add, fetch and summarize; you approve the plan before it runs, and undo takes it all back |
| `:search <query>` | show only the references the query matches, in the library column; the same as typing it after the search key |
| `:save <name>` | save the current search |
| `:summarize[!] [table] [--llm <profile>]` | fill the reference's table row with an LLM; ! overwrites |
| `:cancel` | stop summarizing after the current paper |
| `:export <table> <file.csv>` | export a table |
| `:attach <file.pdf> [location]` | add a PDF, copying or uploading it to a location |
| `:new` | add a reference by hand, into the selected collection; saved on Enter |
| `:cache` | download the selected or marked references' WebDAV PDFs that are missing here or changed on the server |
| `:detach` | remove one of the selected reference's PDFs with its annotations; the file stays where it is |
| `:sync` | commit, pull and push the library with git |
| `:diff` | the library's changes since the last :sync; v marks, d d undoes the selected or marked files |
| `:git-init [url]` | make the library folder a git repository and commit it; with a URL, push it there as origin |
| `:graph[!] [citations]` | citation graph of the selected or marked references, from OpenAlex (:help explains it) |
| `:graph tags` | the selected or marked references and their tags, clustered |
| `:graph col <column>` | the same with a table column's values, such as :graph col group |
| `:discover[!] [--like] [--llm <profile>] [what]` | ask the LLM for papers like the marked ones (--like), or as described, searching the web and arXiv; with neither it opens a form; ! skips cached results; v marks, a adds |
| `:found` | the papers the last :discover found |
| `:history` | past commands; Enter puts one on the command line (Ctrl-R there) |
| `:setup` | list everything :setup sets, as it is set now; Enter changes one |
| `:setup provider\|llm\|webdav [name]` | add or change a provider, an LLM profile or a WebDAV location in yeda.toml, testing it first |
| `:setup openalex` | set the optional OpenAlex API key, testing it first |
| `:setup web-search [keenable\|exa\|parallel\|firecrawl\|searxng]` | the search providers :discover uses, or one provider's optional key or SearXNG URL, testing it first |
| `:setup proxy` | set the library proxies :fetch opens papers through |
| `:setup table [name]` | add or change a literature table: its columns and what each holds |
| `:config [library\|device]` | edit yeda.toml, or this device's config.toml, in $EDITOR |
| `:secrets` | every password, API key and OAuth token the config uses: where each is set; set, replace or remove them |
| `:envvars` | the environment variable that sets each secret, and which ones need an alias to tell them apart |
| `:graph close` | close the graph shown |
| `:table [name]` | a table of the selected or marked references; without a name, pick one (:help explains it) |
| `:table close` | close the table |
| `:table rename <old> <new>` | rename a table in yeda.toml and in every paper's LLM file |
| `:cite [format]` | copy the selected or marked references: bibtex, ident (DOI, arXiv ID, ISBN or URL), a style (ieee, apa, …) or a .csl file |
| `:fetch [location]` | find and attach PDFs for the selected or marked references (:help explains it) |
| `:keys` | this list |
| `:help` | guide: search syntax (AND, OR, NOT, fields), filters, saved searches |
| `:failed` | what the last add or fetch could not get, with the reasons |
| `:log` | every typed command, status message and failure, with times, in $EDITOR |
| `:q` | quit |
| `:<action>` | any action above, such as :annotations |
| `mouse` | a click selects and focuses; the wheel moves the selection, or scrolls the graph |
| `narrow` | below 120 columns the right column is hidden, below 80 the library column too |
| `--name value` | an option, anywhere in a command (also --name=value); -- ends options, so the rest is text |
| `keys` | change them under [keys.<context>] in ~/.config/yeda/config.toml (:help shows how) |
