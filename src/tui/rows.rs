use super::*;

pub(super) fn spans_with_styles(
    shown: &str,
    positions: &[u32],
    plain: Style,
    highlight: Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_highlighted = false;
    let mut next = positions.iter().peekable();
    for (i, ch) in shown.chars().enumerate() {
        while next.next_if(|&&p| (p as usize) < i).is_some() {}
        let highlighted = next.peek().is_some_and(|&&p| p as usize == i);
        if highlighted != run_highlighted && !run.is_empty() {
            let text = std::mem::take(&mut run);
            spans.push(Span::styled(
                text,
                if run_highlighted { highlight } else { plain },
            ));
        }
        run_highlighted = highlighted;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(
            run,
            if run_highlighted { highlight } else { plain },
        ));
    }
    spans
}

/// The first regex match in `line`, split into plain spans around an
/// accent-styled match span.
pub(super) fn highlight_first_match(
    line: &str,
    re: &regex::Regex,
    accent: Style,
) -> Vec<Span<'static>> {
    let Some(m) = re.find(line) else {
        return vec![Span::raw(line.to_string())];
    };
    let (start, end) = (m.start(), m.end());
    let mut spans = Vec::new();
    if start > 0 {
        spans.push(Span::raw(line[..start].to_string()));
    }
    spans.push(Span::styled(line[start..end].to_string(), accent));
    if end < line.len() {
        spans.push(Span::raw(line[end..].to_string()));
    }
    spans
}

/// (label, color) for the little kind badge in front of a row; `badges` is
/// the theme's [image, video/audio, doc, code, archive, other] palette and
/// `accent` colors directory badges.
pub(super) fn badge_for(path: &str, badges: [Color; 6], accent: Color) -> (String, Color) {
    if path.ends_with('/') {
        return ("DIR".to_string(), accent);
    }
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    if ext.is_empty() {
        return ("FILE".to_string(), badges[5]);
    }
    let label: String = ext.chars().take(4).collect::<String>().to_uppercase();
    let color = match crate::filters::kind_for_ext(ext) {
        Some("image") => badges[0],
        Some("video") | Some("audio") => badges[1],
        Some("doc") => badges[2],
        Some("code") => badges[3],
        Some("archive") => badges[4],
        _ => badges[5],
    };
    (label, color)
}

/// The badge span (`" PDF "` on its kind color) plus the gap space after it,
/// and the total visual width of both (used to indent second lines).
pub(super) fn badge_spans(
    path: &str,
    badges: [Color; 6],
    accent: Color,
) -> (Vec<Span<'static>>, usize) {
    let (label, color) = badge_for(path, badges, accent);
    let width = label.chars().count() + 3; // " label " + the gap
    let span = Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    );
    (vec![span, Span::raw(" ")], width)
}

/// Spaces to push the right column flush against the row's right edge; None
/// when there is no room for even one gap (callers then drop the column).
pub(super) fn right_pad(left: usize, right: usize, inner_width: usize) -> Option<usize> {
    let pad = inner_width.saturating_sub(left + right);
    (pad >= 1).then_some(pad)
}

/// "5m ago" for a row's mtime; None when the meta is missing or bogus
/// (mtime <= 0).
pub(super) fn row_age(meta: Option<FileMeta>) -> Option<String> {
    meta.filter(|m| m.mtime > 0)
        .map(|m| human_age(SystemTime::UNIX_EPOCH + Duration::from_secs(m.mtime as u64)))
}

/// Filled/empty cells of a 5-cell score bar for a 0..=1 similarity score.
/// Returns the number filled and the bar glyph string.
pub(super) fn score_bar(s: f32) -> (usize, String) {
    let filled = ((s.clamp(0.0, 1.0) * 5.0).round() as usize).min(5);
    (filled, "▰".repeat(filled) + &"▱".repeat(5 - filled))
}

/// Right-aligned score readout for a semantic row: a styled 5-cell bar plus
/// the percent, returning its cell width and the spans to render.
pub(super) fn score_readout(s: f32, accent: Color, dim: Style) -> (usize, Vec<Span<'static>>) {
    // build the two segments directly: the bar glyphs are 3 bytes each, so
    // splitting the joined string at the fill COUNT would land mid-char
    let (filled, _) = score_bar(s);
    let fill = "\u{25b0}".repeat(filled);
    let rest = "\u{25b1}".repeat(5 - filled);
    let pct = format!(" {:.0}%", s * 100.0);
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !fill.is_empty() {
        spans.push(Span::styled(fill, Style::default().fg(accent)));
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest, dim));
    }
    let width = 5 + pct.chars().count();
    spans.push(Span::styled(pct, dim));
    (width, spans)
}

pub(super) fn draw_results(frame: &mut Frame, app: &mut App, area: Rect) {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let match_color = app.theme.match_fg.unwrap_or(app.theme.accent);
    let accent = Style::default()
        .fg(match_color)
        .add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(app.theme.dim);
    let badges = app.theme.badges;
    let dir_color = app.theme.accent;
    // take the cached highlighter out so the results borrow doesn't block
    // rebuilding it; it goes back on App before the frame renders
    let mut highlighter = std::mem::take(&mut app.highlighter);
    if matches!(app.engine.mode(), Mode::Fuzzy) && !app.input.is_empty() {
        if app.highlighter_input != app.input {
            app.highlighter_input = app.input.clone();
            highlighter = Some(Highlighter::new(&app.input));
        }
    } else {
        app.highlighter_input.clear();
        highlighter = None;
    }
    // same take/rebuild cache for the first-match content highlight
    let mut content_re = std::mem::take(&mut app.content_highlight);
    if matches!(app.engine.mode(), Mode::Content) && !app.input.is_empty() {
        if app.content_highlight_input != app.input {
            app.content_highlight_input = app.input.clone();
            let (_, pattern) = crate::engine::parse_query(&app.input, app.regex_mode);
            content_re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(!pattern.chars().any(char::is_uppercase))
                .build()
                .ok();
        }
    } else {
        app.content_highlight_input.clear();
        content_re = None;
    }
    let inner_width = area.width.saturating_sub(2) as usize; // minus the borders
    let name_plain = Style::default().add_modifier(Modifier::BOLD);
    let parent_hl = Style::default().fg(match_color).add_modifier(Modifier::DIM);
    let items: Vec<ListItem> = app
        .engine
        .results()
        .iter()
        .take(app.visible_len())
        .map(|r| {
            if app.engine.is_filter() {
                // filter mode: one plain line per row — the raw line text
                // with fuzzy-match highlights; no home shortening, badge,
                // name/parent split, or size/age columns
                let positions: Vec<u32> = match highlighter.as_mut() {
                    Some(hl) => hl.positions(&r.path),
                    None => Vec::new(),
                };
                return ListItem::new(Line::from(spans_with_styles(
                    &r.path,
                    &positions,
                    Style::default(),
                    accent,
                )));
            }
            let (shown, trimmed_chars) = match &home {
                Some(h) if r.path.starts_with(h.as_str()) => {
                    (format!("~{}", &r.path[h.len()..]), h.chars().count())
                }
                _ => (r.path.clone(), 0),
            };
            // split the shown path at the name boundary: the final component
            // (trailing '/' kept for directories) is the hero, the rest is
            // the dim parent
            let name = {
                let stem = r.path.trim_end_matches('/');
                let last = stem.rsplit('/').next().unwrap_or("");
                if r.path.ends_with('/') {
                    format!("{last}/")
                } else {
                    last.to_string()
                }
            };
            let parent = shown[..shown.len() - name.len()].to_string();
            let name_chars = name.chars().count();
            let parent_chars = shown.chars().count() - name_chars;
            match (r.line_number, &r.line) {
                (Some(n), Some(line)) => {
                    let (badge, badge_width) = badge_spans(&r.path, badges, dir_color);
                    let colon = format!(":{n}");
                    let age = row_age(r.meta);
                    match app.density {
                        Density::Comfy => ListItem::new(Text::from(vec![
                            // badge, bold name, dim :n, age flush right
                            Line::from({
                                let mut spans = badge;
                                spans.push(Span::styled(name.clone(), name_plain));
                                spans.push(Span::styled(colon.clone(), dim));
                                // semantic rows show a score bar; others call out age
                                let right = match r.score {
                                    Some(s) => Some(score_readout(s, app.theme.accent, dim)),
                                    None => age.as_ref().map(|a| {
                                        (a.chars().count(), vec![Span::styled(a.clone(), dim)])
                                    }),
                                };
                                if let Some((right_width, right_spans)) = right {
                                    let left = badge_width + name_chars + colon.chars().count();
                                    if let Some(pad) = right_pad(left, right_width, inner_width) {
                                        spans.push(Span::raw(" ".repeat(pad)));
                                        spans.extend(right_spans);
                                    }
                                }
                                spans
                            }),
                            // indented matched line text
                            Line::from({
                                let mut spans = vec![Span::raw(" ".repeat(badge_width))];
                                match &content_re {
                                    Some(re) => {
                                        spans.extend(highlight_first_match(line, re, accent))
                                    }
                                    None => spans.push(Span::raw(line.clone())),
                                }
                                spans
                            }),
                        ])),
                        Density::Compact => ListItem::new(Line::from({
                            let mut spans = badge;
                            spans.push(Span::styled(name.clone(), name_plain));
                            spans.push(Span::styled(format!("{colon} "), dim));
                            match &content_re {
                                Some(re) => spans.extend(highlight_first_match(line, re, accent)),
                                None => spans.push(Span::raw(line.clone())),
                            }
                            // score rows right-align the bar after the snippet
                            if let Some(s) = r.score {
                                let (right_width, right_spans) =
                                    score_readout(s, app.theme.accent, dim);
                                let left = badge_width
                                    + name_chars
                                    + colon.chars().count()
                                    + 1
                                    + line.chars().count();
                                if let Some(pad) = right_pad(left, right_width, inner_width) {
                                    spans.push(Span::raw(" ".repeat(pad)));
                                    spans.extend(right_spans);
                                }
                            }
                            spans
                        })),
                    }
                }
                _ => {
                    let (badge, badge_width) = badge_spans(&r.path, badges, dir_color);
                    // positions refer to the full path; shift them onto the
                    // `~`-shortened string, then partition them at the name
                    // boundary (the parent starts at index 0 of `shown`)
                    let (in_name, in_parent): (Vec<u32>, Vec<u32>) = match highlighter.as_mut() {
                        Some(hl) => {
                            let shift = if trimmed_chars > 0 {
                                trimmed_chars - 1
                            } else {
                                0
                            };
                            let positions: Vec<u32> = hl
                                .positions(&r.path)
                                .into_iter()
                                .filter(|&p| p as usize >= trimmed_chars)
                                .map(|p| (p as usize - shift) as u32)
                                .collect();
                            positions
                                .into_iter()
                                .partition(|&p| p as usize >= parent_chars)
                        }
                        None => (Vec::new(), Vec::new()),
                    };
                    let name_positions: Vec<u32> = in_name
                        .into_iter()
                        .map(|p| p - parent_chars as u32)
                        .collect();
                    let name_spans = spans_with_styles(&name, &name_positions, name_plain, accent);
                    match app.density {
                        Density::Comfy => {
                            // line 1: badge, bold name (highlights), age flush right
                            let age = row_age(r.meta);
                            let mut line1 = badge;
                            line1.extend(name_spans);
                            if let Some(age) = &age {
                                let left = badge_width + name_chars;
                                if let Some(pad) = right_pad(left, age.chars().count(), inner_width)
                                {
                                    line1.push(Span::raw(" ".repeat(pad)));
                                    line1.push(Span::styled(age.clone(), dim));
                                }
                            }
                            // line 2: indented dim parent (+ size when known)
                            let mut line2 = vec![Span::raw(" ".repeat(badge_width))];
                            line2.extend(spans_with_styles(&parent, &in_parent, dim, parent_hl));
                            if let Some(size) = r
                                .meta
                                .filter(|_| !r.path.ends_with('/'))
                                .map(|m| format!(" · {}", human_size(m.size)))
                            {
                                line2.push(Span::raw(size));
                            }
                            ListItem::new(Text::from(vec![Line::from(line1), Line::from(line2)]))
                        }
                        Density::Compact => {
                            // badge, bold name, dim " — parent", size/age right
                            let right = r.meta.map(|m| {
                                let size = human_size(m.size);
                                match row_age(Some(m)) {
                                    Some(age) => format!("{size} · {age}"),
                                    None => size,
                                }
                            });
                            let mut line1 = badge;
                            line1.extend(name_spans);
                            line1.push(Span::styled(" — ".to_string(), dim));
                            line1.extend(spans_with_styles(&parent, &in_parent, dim, parent_hl));
                            if let Some(right) = &right {
                                let left = badge_width + name_chars + 3 + parent.chars().count();
                                if let Some(pad) =
                                    right_pad(left, right.chars().count(), inner_width)
                                {
                                    line1.push(Span::raw(" ".repeat(pad)));
                                    line1.push(Span::styled(right.clone(), dim));
                                }
                            }
                            ListItem::new(Line::from(line1))
                        }
                    }
                }
            }
        })
        .collect();
    // On the launch screen (empty query), split the list into "recent
    // opens" (frecency) and "recently modified" with dim section headers.
    // Headers are extra list rows, so the selection index shifts past them.
    let rows = app.engine.results();
    let visible = app.visible_len();
    let strong = app.engine.strong_count();
    let hidden = rows.len().saturating_sub(visible);
    let opened = rows.iter().take_while(|r| r.recent_open).count();
    let sectioned = app.input.is_empty() && matches!(app.engine.mode(), Mode::Fuzzy) && opened > 0;
    let mut display_items = items;
    let mut display_selected = app.selected;
    // slot map mirrors the final display list 1:1 for mouse hit testing:
    // result rows are 2 lines in Comfy density, 1 in Compact (content rows
    // are 2 lines in comfy too); filter rows are always 1 line. headers and
    // the fold row are 1 line each.
    let row_height: u16 = if app.engine.is_filter() {
        1
    } else {
        match app.density {
            Density::Comfy => 2,
            Density::Compact => 1,
        }
    };
    let mut slots: Vec<(Slot, u16)> = (0..visible).map(|i| (Slot::Row(i), row_height)).collect();
    if sectioned {
        let header = |label: &str| {
            ListItem::new(Span::styled(
                format!("─ {label} ────────"),
                Style::default().fg(app.theme.section.unwrap_or(app.theme.dim)),
            ))
        };
        let mut with_headers = Vec::with_capacity(display_items.len() + 2);
        let mut with_slots = Vec::with_capacity(slots.len() + 2);
        with_headers.push(header("RECENT OPENS"));
        with_slots.push((Slot::Header, 1));
        for (i, item) in display_items.into_iter().enumerate() {
            if i == opened {
                with_headers.push(header("RECENTLY MODIFIED"));
                with_slots.push((Slot::Header, 1));
            }
            with_headers.push(item);
            with_slots.push(slots[i]);
        }
        display_items = with_headers;
        slots = with_slots;
        display_selected += if app.selected < opened { 1 } else { 2 };
    }
    // The weaker-match fold: a one-line dim fold after the last strong row
    // while weak matches are hidden, or — once revealed — a section header
    // before the first weaker row. Extra non-result rows (like the launch
    // sections above); the trailing fold row never shifts the selection.
    if hidden > 0 || (app.show_weak && strong < rows.len()) {
        let mut out = Vec::with_capacity(display_items.len() + 2);
        if app.show_weak && strong < rows.len() {
            for (i, item) in display_items.into_iter().enumerate() {
                if i == strong {
                    out.push(ListItem::new(Span::styled(
                        "─ WEAKER MATCHES ─",
                        Style::default().fg(app.theme.section.unwrap_or(app.theme.dim)),
                    )));
                    slots.insert(i, (Slot::Header, 1));
                }
                out.push(item);
            }
            display_items = out;
            // the header sits just before the first weaker row; a selection
            // at or past it shifts one place
            display_selected += if app.selected < strong { 0 } else { 1 };
        } else {
            out.extend(display_items);
            out.push(ListItem::new(Span::styled(
                format!("▸ {hidden} weaker matches hidden · ctrl-x show"),
                Style::default().fg(app.theme.dim),
            )));
            display_items = out;
            slots.push((Slot::Fold, 1));
        }
    }
    app.highlighter = highlighter;
    let block = themed_block("results", &app.theme);
    app.results_area = block.inner(area);
    let list = List::new(display_items)
        .block(block)
        .highlight_style(selection_style(&app.theme));
    app.list_state.select(Some(display_selected));
    frame.render_stateful_widget(list, area, &mut app.list_state);
    app.slots = slots;
}

/// `path` with a home-directory prefix shortened to `~`; unchanged otherwise.
pub(super) fn shorten_home(path: &str) -> String {
    match dirs::home_dir() {
        Some(h) => {
            let h = h.to_string_lossy();
            if path.starts_with(h.as_ref()) {
                format!("~{}", &path[h.len()..])
            } else {
                path.to_string()
            }
        }
        None => path.to_string(),
    }
}

/// Final path component, trailing '/' kept for directories.
pub(super) fn path_name(path: &str) -> String {
    let stem = path.trim_end_matches('/');
    let last = stem.rsplit('/').next().unwrap_or("");
    if path.ends_with('/') {
        format!("{last}/")
    } else {
        last.to_string()
    }
}

/// Metadata for the selected row: its own index meta, else the status-line
/// stat cache when that covers the same path.
pub(super) fn kind_label(path: &str) -> String {
    if path.ends_with('/') {
        "DIR".to_string()
    } else {
        std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("FILE")
            .to_uppercase()
    }
}
