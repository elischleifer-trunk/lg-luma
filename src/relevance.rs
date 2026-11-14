//! Ranks streaming search results for what this household watches: commercial work from US
//! studios. Movie of the Night says where a title streams; TMDB says how prominent it is.
//!
//! 1. Filter: adult titles, little-seen titles (few TMDB votes) and titles that are neither in
//!    English nor from the US are dropped, unless the query names them (*Amélie*). "From the
//!    US" goes by country of origin, so an anime with an American dub co-producer is Japanese.
//!    English-language work from elsewhere (BBC *Ghosts*) stays, weighted lower.
//! 2. Match: how the title matches the query — exact or leading words, then a prefix, then a
//!    whole word, then anywhere. Franchise sequels join their first film's group even when
//!    their titles differ.
//! 3. Score within each match group: major studio or network, US production, how widely seen
//!    (votes), budget and box office, a little for current popularity.
//! 4. Franchises stay together, oldest first: "ghostbusters" lists the five films from 1984.
//! 5. Where it streams on this TV is only a tiebreaker, and is shown on the card.
//!
//! Pure logic over already-fetched data, so it is tested against captured fixtures.
use crate::motn_api::{Show, ShowType};
use crate::streaming::{self, Access, Ranked, Services};
use crate::tmdb_api::{MovieDetailsResponse, SearchMultiResponseResult, TvSeriesDetailsResponse};
use std::collections::HashMap;

/// Fewer TMDB votes than this means a title few people have seen. Series collect fewer votes
/// than films, so their bar is lower.
pub const MIN_MOVIE_VOTES: i64 = 100;
pub const MIN_SERIES_VOTES: i64 = 30;

/// TMDB company IDs of major US studios and their labels (each checked against TMDB).
pub const MAJOR_STUDIOS: &[(i64, &str)] = &[
    (1, "Lucasfilm"),
    (2, "Walt Disney Pictures"),
    (3, "Pixar"),
    (4, "Paramount Pictures"),
    (5, "Columbia Pictures"),
    (7, "DreamWorks Pictures"),
    (12, "New Line Cinema"),
    (21, "Metro-Goldwyn-Mayer"),
    (25, "20th Century Fox"),
    (33, "Universal Pictures"),
    (34, "Sony Pictures"),
    (35, "Lions Gate Films"),
    (43, "Fox Searchlight Pictures"),
    (174, "Warner Bros. Pictures"),
    (420, "Marvel Studios"),
    (429, "DC"),
    (521, "DreamWorks Animation"),
    (559, "TriStar Pictures"),
    (923, "Legendary Pictures"),
    (1632, "Lionsgate"),
    (2251, "Sony Pictures Animation"),
    (2785, "Warner Bros. Animation"),
    (6125, "Walt Disney Animation Studios"),
    (6704, "Illumination"),
    (9993, "DC Entertainment"),
    (10146, "Focus Features"),
    (20580, "Amazon Studios"),
    (41077, "A24"),
    (127928, "20th Century Studios"),
    (210099, "Amazon MGM Studios"),
    // Television studios.
    (1081, "CBS Studios"),
    (1957, "Warner Bros. Television"),
    (3268, "HBO"),
    (3614, "Columbia Pictures Television"),
    (9223, "Paramount Television"),
    (10471, "Columbia TriStar Television"),
    (11073, "Sony Pictures Television"),
    (19366, "ABC Studios"),
    (26727, "Universal Television"),
];

/// TMDB network IDs of major US networks and streaming services (each checked against TMDB).
pub const MAJOR_NETWORKS: &[(i64, &str)] = &[
    (2, "ABC"),
    (6, "NBC"),
    (13, "Nickelodeon"),
    (16, "CBS"),
    (19, "FOX"),
    (30, "USA Network"),
    (44, "Disney XD"),
    (49, "HBO"),
    (54, "Disney Channel"),
    (56, "Cartoon Network"),
    (64, "Discovery"),
    (67, "Showtime"),
    (71, "The CW"),
    (88, "FX"),
    (174, "AMC"),
    (213, "Netflix"),
    (318, "STARZ"),
    (453, "Hulu"),
    (1024, "Prime Video"),
    (2552, "Apple TV"),
    (2739, "Disney+"),
    (3353, "Peacock"),
    (4330, "Paramount+"),
    (6783, "Max"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Movie,
    Series,
}

/// A title's TMDB identity, from Movie of the Night's `tmdbId` (`movie/620`, `tv/2286`).
pub fn tmdb_id(show: &Show) -> Option<(Kind, i64)> {
    let (kind, id) = show.tmdb_id.split_once('/')?;
    let kind = match kind {
        "movie" => Kind::Movie,
        "tv" => Kind::Series,
        _ => return None,
    };
    Some((kind, id.parse().ok()?))
}

/// What TMDB says about one title: a search result's summary, refined by its details.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signals {
    pub adult: bool,
    pub votes: i64,
    pub popularity: f64,
    pub language: String,
    /// Countries of origin (ISO 3166-1): where the work comes from.
    pub origin: Vec<String>,
    /// Production countries, which also list co-producers (an English dub of an anime).
    pub countries: Vec<String>,
    pub companies: Vec<i64>,
    pub networks: Vec<i64>,
    pub budget: i64,
    pub revenue: i64,
    pub collection: Option<i64>,
    /// Release date for films, first air date for series (`YYYY-MM-DD`), when known.
    pub date: String,
    /// The US rating (PG-13, TV-14), from details fetched with `append_to_response`.
    pub rating: Option<String>,
    /// Other titles used in the US and UK ("Star Wars: Andor"), from details fetched with
    /// `append_to_response=alternative_titles`.
    pub titles: Vec<String>,
}

/// Countries whose alternative titles count as English names for the title.
const ENGLISH_TITLE_COUNTRIES: &[&str] = &["US", "GB"];

fn english_titles<'a>(
    titles: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> Vec<String> {
    titles
        .filter(|(country, _)| country.is_some_and(|c| ENGLISH_TITLE_COUNTRIES.contains(&c)))
        .filter_map(|(_, title)| title.map(str::to_owned))
        .collect()
}

/// The US certification of a film: the theatrical one when set, else any US release's.
pub fn us_certification(details: &MovieDetailsResponse) -> Option<String> {
    let us = details
        .release_dates
        .as_ref()?
        .results
        .iter()
        .flatten()
        .find(|result| result.iso_3166_1.as_deref() == Some("US"))?;
    let dates = us.release_dates.iter().flatten();
    let rated = |date: &&crate::tmdb_api::MovieDetailsResponseReleaseDatesResultReleaseDate| {
        date.certification
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty())
    };
    dates
        .clone()
        .filter(rated)
        .find(|date| date.r#type == Some(3))
        .or_else(|| dates.clone().find(rated))
        .and_then(|date| date.certification.as_deref())
        .map(|c| c.trim().to_owned())
}

/// The US TV rating of a series.
pub fn us_tv_rating(details: &TvSeriesDetailsResponse) -> Option<String> {
    details
        .content_ratings
        .as_ref()?
        .results
        .iter()
        .flatten()
        .find(|result| result.iso_3166_1.as_deref() == Some("US"))?
        .rating
        .as_deref()
        .map(str::trim)
        .filter(|rating| !rating.is_empty())
        .map(str::to_owned)
}

impl Signals {
    pub fn from_search(result: &SearchMultiResponseResult) -> Self {
        Self {
            adult: result.adult.unwrap_or(false),
            votes: result.vote_count.unwrap_or(0),
            popularity: result.popularity.unwrap_or(0.),
            language: result.original_language.clone().unwrap_or_default(),
            date: result.release_date.clone().unwrap_or_default(),
            ..Self::default()
        }
    }

    pub fn movie(details: &MovieDetailsResponse) -> Self {
        let countries = details
            .production_countries
            .iter()
            .flatten()
            .filter_map(|c| c.iso_3166_1.clone())
            .collect();
        Self {
            adult: details.adult.unwrap_or(false),
            votes: details.vote_count.unwrap_or(0),
            popularity: details.popularity.unwrap_or(0.),
            language: details.original_language.clone().unwrap_or_default(),
            origin: details.origin_country.clone().unwrap_or_default(),
            countries,
            companies: details
                .production_companies
                .iter()
                .flatten()
                .filter_map(|c| c.id)
                .collect(),
            networks: Vec::new(),
            budget: details.budget.unwrap_or(0),
            revenue: details.revenue.unwrap_or(0),
            collection: details.belongs_to_collection.as_ref().and_then(|c| c.id),
            date: details.release_date.clone().unwrap_or_default(),
            rating: us_certification(details),
            titles: english_titles(
                details
                    .alternative_titles
                    .iter()
                    .flat_map(|a| a.titles.iter().flatten())
                    .map(|t| (t.iso_3166_1.as_deref(), t.title.as_deref())),
            ),
        }
    }

    pub fn series(details: &TvSeriesDetailsResponse) -> Self {
        let countries = details
            .production_countries
            .iter()
            .flatten()
            .filter_map(|c| c.iso_3166_1.clone())
            .collect();
        Self {
            adult: details.adult.unwrap_or(false),
            votes: details.vote_count.unwrap_or(0),
            popularity: details.popularity.unwrap_or(0.),
            language: details.original_language.clone().unwrap_or_default(),
            origin: details.origin_country.clone().unwrap_or_default(),
            countries,
            companies: details
                .production_companies
                .iter()
                .flatten()
                .filter_map(|c| c.id)
                .collect(),
            networks: details
                .networks
                .iter()
                .flatten()
                .filter_map(|n| n.id)
                .collect(),
            budget: 0,
            revenue: 0,
            collection: None,
            date: details.first_air_date.clone().unwrap_or_default(),
            rating: us_tv_rating(details),
            titles: english_titles(
                details
                    .alternative_titles
                    .iter()
                    .flat_map(|a| a.results.iter().flatten())
                    .map(|t| (t.iso_3166_1.as_deref(), t.title.as_deref())),
            ),
        }
    }

    /// Made in the US: by country of origin, or production country when TMDB has no origin.
    pub fn american(&self) -> bool {
        let countries = if self.origin.is_empty() {
            &self.countries
        } else {
            &self.origin
        };
        countries.iter().any(|c| c == "US")
    }

    /// Neither in English nor from the US.
    pub fn foreign(&self) -> bool {
        !self.american() && self.language != "en"
    }

    fn major(&self) -> bool {
        self.companies
            .iter()
            .any(|id| MAJOR_STUDIOS.iter().any(|(major, _)| major == id))
            || self
                .networks
                .iter()
                .any(|id| MAJOR_NETWORKS.iter().any(|(major, _)| major == id))
    }

    /// Whether the title belongs in results at all. Foreign work is kept only when the query
    /// names it (`named`): searching "amélie" still finds *Amélie*.
    pub fn keep(&self, kind: Kind, named: bool) -> bool {
        let min = match kind {
            Kind::Movie => MIN_MOVIE_VOTES,
            Kind::Series => MIN_SERIES_VOTES,
        };
        !self.adult && self.votes >= min && (named || !self.foreign())
    }

    /// Higher is more likely to be what the household wants.
    pub fn score(&self) -> f64 {
        let millions = |amount: i64| (1. + amount.max(0) as f64 / 1e6).ln();
        f64::from(u8::from(self.major())) * 3.
            + f64::from(u8::from(self.american())) * 2.
            + (1. + self.votes.max(0) as f64).ln()
            + 0.5 * millions(self.budget)
            + 0.5 * millions(self.revenue)
            + 0.2 * (1. + self.popularity.max(0.)).ln()
    }
}

/// TMDB data for one search: summaries from `search/multi` and details for some titles.
#[derive(Clone, Debug, Default)]
pub struct Enrichment {
    pub signals: HashMap<(Kind, i64), Signals>,
}

impl Enrichment {
    pub fn add_search(&mut self, results: &[SearchMultiResponseResult]) {
        for result in results {
            let kind = match result.media_type.as_deref() {
                Some("movie") => Kind::Movie,
                Some("tv") => Kind::Series,
                _ => continue,
            };
            if let Some(id) = result.id {
                // Details are richer; never let a summary replace them.
                self.signals
                    .entry((kind, id))
                    .or_insert_with(|| Signals::from_search(result));
            }
        }
    }

    pub fn add_movie(&mut self, details: &MovieDetailsResponse) {
        if let Some(id) = details.id {
            self.signals
                .insert((Kind::Movie, id), Signals::movie(details));
        }
    }

    // The relevance tests build enrichment from series fixtures.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn add_series(&mut self, details: &TvSeriesDetailsResponse) {
        if let Some(id) = details.id {
            self.signals
                .insert((Kind::Series, id), Signals::series(details));
        }
    }

    fn get(&self, show: &Show) -> Option<&Signals> {
        self.signals.get(&tmdb_id(show)?)
    }
}

/// A half-typed word is completed only when the word it would become has at least this share
/// of the votes of all the titles it could be.
pub const COMPLETION_SHARE: f64 = 0.8;

/// A letter or digit as it is compared: lowercase input, accents removed. None for anything
/// that separates words.
fn fold(c: char) -> Option<char> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        c if c.is_alphanumeric() => c,
        _ => return None,
    })
}

/// Each word of `text` as it is compared (see [`fold`]) and as it is written, in lowercase.
fn tokens(text: &str) -> Vec<(String, String)> {
    let mut tokens = Vec::new();
    let (mut folded, mut written) = (String::new(), String::new());
    for c in text.to_lowercase().chars() {
        match fold(c) {
            Some(plain) => {
                folded.push(plain);
                written.push(c);
            }
            None if folded.is_empty() => {}
            None => tokens.push((std::mem::take(&mut folded), std::mem::take(&mut written))),
        }
    }
    if !folded.is_empty() {
        tokens.push((folded, written));
    }
    tokens
}

/// Lowercase words without punctuation or accents, so "Amélie" matches "amelie".
fn words(text: &str) -> Vec<String> {
    tokens(text).into_iter().map(|(folded, _)| folded).collect()
}

/// The typed text with its unfinished last word finished: "ghostb" → "ghostbusters",
/// "star w" → "star wars". Movie of the Night matches whole words, so searching it for half a
/// word spends a metered call on near misses; TMDB's free search does match a word's start.
/// The word chosen is the one the most-seen matching titles continue with, and only when they
/// nearly all do ([`COMPLETION_SHARE`]): "gho" could be *Ghost*, *Ghosts* or *Ghostbusters*,
/// so it is searched as typed. None too when the text already ends in a whole word of a title
/// worth listing ("super", "fan"), or when no such title starts a word with it ("sup": TMDB
/// lists exact words first, and those are obscure).
///
/// Every keystroke that completes to the same word is one cached search, so typing the rest
/// of "ghostbusters" costs nothing and the results do not move.
pub fn complete(query: &str, results: &[SearchMultiResponseResult]) -> Option<String> {
    let typed = words(query);
    let (last, leading) = typed.split_last()?;
    // Punctuation after the last word means it is finished.
    query.chars().next_back().and_then(fold)?;
    // Completed word → (as written in the title, votes of the titles that use it).
    let mut english: Vec<(String, String, i64)> = Vec::new();
    let mut foreign: Vec<(String, String, i64)> = Vec::new();
    let mut foreign_whole = false;
    for result in results {
        let kind = match result.media_type.as_deref() {
            Some("movie") => Kind::Movie,
            Some("tv") => Kind::Series,
            _ => continue,
        };
        let Some(title) = result.title.as_deref().or(result.name.as_deref()) else {
            continue;
        };
        let signals = Signals::from_search(result);
        if !signals.keep(kind, true) {
            continue;
        }
        let title = tokens(title);
        let starts = |window: &[(String, String)]| {
            window[..leading.len()]
                .iter()
                .map(|(folded, _)| folded)
                .eq(leading)
                && window[leading.len()].0.starts_with(last.as_str())
        };
        let mut matches = title.windows(typed.len()).filter(|window| starts(window));
        if matches
            .clone()
            .any(|window| window[leading.len()].0 == *last)
        {
            if signals.foreign() {
                foreign_whole = true;
                continue;
            }
            return None;
        }
        let Some(window) = matches.next() else {
            continue;
        };
        let (folded, written) = &window[leading.len()];
        let words = if signals.foreign() {
            &mut foreign
        } else {
            &mut english
        };
        match words.iter_mut().find(|(word, _, _)| word == folded) {
            Some((_, _, votes)) => *votes += signals.votes,
            None => words.push((folded.clone(), written.clone(), signals.votes)),
        }
    }
    let words = match (english.is_empty(), foreign_whole) {
        (false, _) => english,
        // The text names a foreign title outright (*Amélie*): search for it as typed.
        (true, true) => return None,
        (true, false) => foreign,
    };
    // The first of equally seen words: TMDB's own order breaks the tie.
    let mut best: Option<&(String, String, i64)> = None;
    for word in &words {
        if best.is_none_or(|(_, _, votes)| word.2 > *votes) {
            best = Some(word);
        }
    }
    let (_, written, votes) = best?;
    let total: i64 = words.iter().map(|(_, _, votes)| votes).sum();
    if (*votes as f64) < COMPLETION_SHARE * total as f64 {
        return None;
    }
    let rest: String = written.chars().skip(last.chars().count()).collect();
    Some(format!("{query}{rest}"))
}

/// How a title matches the query; lower is better.
pub fn match_tier(query: &str, title: &str) -> u8 {
    let query = words(query);
    let title = words(title);
    if query.is_empty() {
        return 4;
    }
    if title.starts_with(&query) {
        return 0;
    }
    let (last, whole) = query.split_last().unwrap();
    let leading = title.len() >= query.len()
        && title[..whole.len()] == *whole
        && title[whole.len()].starts_with(last.as_str());
    if leading {
        return 1;
    }
    if title.windows(query.len()).any(|window| window == query) {
        return 2;
    }
    // Punctuation counts as nothing as well as a space: "oceans" finds "Ocean's Eleven",
    // "spiderman" finds "Spider-Man".
    if title.join(" ").contains(&query.join(" ")) || title.concat().contains(&query.concat()) {
        return 3;
    }
    4
}

/// How the best of a show's English names matches the query: its title, or an alternative
/// title used in the US or UK. *Andor* is "Star Wars: Andor" there.
fn best_tier(query: &str, show: &Show, signals: &Signals) -> u8 {
    signals
        .titles
        .iter()
        .map(|title| match_tier(query, title))
        .fold(match_tier(query, &show.title), u8::min)
}

/// "Ghostbusters: Afterlife" and "Ghostbusters II" share the base title "ghostbusters".
fn base_title(title: &str) -> String {
    let main = title.split([':', '(']).next().unwrap_or(title);
    let mut words = words(main.split(" - ").next().unwrap_or(main));
    while words.len() > 1
        && words.last().is_some_and(|w| {
            w.chars().all(|c| c.is_ascii_digit())
                || matches!(w.as_str(), "ii" | "iii" | "iv" | "v" | "vi" | "part")
        })
    {
        words.pop();
    }
    words.join(" ")
}

struct Candidate {
    show: Show,
    kind: Kind,
    tier: u8,
    score: f64,
    access: u8,
    collection: Option<i64>,
    base: String,
    american: bool,
    date: String,
    /// The original title matches the query (*Amélie*).
    original: bool,
}

/// Same franchise: same kind and match group, and the same TMDB collection, or the same base
/// title from the same side of the Atlantic. A US remake of a British show is not its sequel:
/// grouping them oldest first would put the British original above it.
fn related(a: &Candidate, b: &Candidate) -> bool {
    a.kind == b.kind
        && a.tier == b.tier
        && ((a.collection.is_some() && a.collection == b.collection)
            || (a.base == b.base && a.american == b.american))
}

/// Orders Movie of the Night results for `query`. Without TMDB data (no token, or TMDB was
/// unreachable) it falls back to [`streaming::rank`]: availability, then the API's order.
pub fn rank(
    query: &str,
    shows: &[Show],
    country: &str,
    services: &Services,
    enrichment: Option<&Enrichment>,
) -> Vec<Ranked> {
    let Some(enrichment) = enrichment else {
        return streaming::rank(shows, country, services);
    };
    let mut candidates: Vec<Candidate> = shows
        .iter()
        .filter_map(|show| {
            let signals = enrichment.get(show)?;
            let kind = match show.show_type {
                ShowType::Series => Kind::Series,
                _ => Kind::Movie,
            };
            let tier = best_tier(query, show, signals);
            if !signals.keep(kind, tier == 0) {
                return None;
            }
            let (pick, _) = streaming::pick(show, country, services);
            Some(Candidate {
                show: show.clone(),
                kind,
                tier,
                score: signals.score(),
                access: match pick.map(|p| p.access) {
                    Some(Access::Included) => 0,
                    Some(_) => 1,
                    None => 2,
                },
                collection: signals.collection,
                base: base_title(&show.title),
                american: signals.american(),
                date: if signals.date.is_empty() {
                    streaming::year(show)
                        .map(|y| y.to_string())
                        .unwrap_or_default()
                } else {
                    signals.date.clone()
                },
                original: match_tier(query, &show.original_title) < 4,
            })
        })
        .collect();

    // Sequels join the group of the best-matching film in their collection.
    let mut collection_tiers: HashMap<i64, u8> = HashMap::new();
    for candidate in &candidates {
        if let Some(collection) = candidate.collection {
            let tier = collection_tiers.entry(collection).or_insert(candidate.tier);
            *tier = (*tier).min(candidate.tier);
        }
    }
    for candidate in &mut candidates {
        if let Some(tier) = candidate.collection.and_then(|c| collection_tiers.get(&c)) {
            candidate.tier = *tier;
        }
    }

    // Movie of the Night matches titles in every language and fills its 20 results with near
    // misses: "super" brings back *8MM* and *The Bad Guys 2*. Drop titles none of whose names
    // contain the query and that share no collection with one that does. When none match,
    // keep them all: a near miss beats an empty screen (a misspelt title).
    let on_topic = |c: &Candidate| c.tier < 4 || c.original;
    if candidates.iter().any(on_topic) {
        candidates.retain(on_topic);
    }

    candidates.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then(b.score.total_cmp(&a.score))
            .then(a.access.cmp(&b.access))
    });

    // Keep each franchise together, oldest first, where its best entry ranked.
    let mut remaining: Vec<Option<Candidate>> = candidates.into_iter().map(Some).collect();
    let mut ordered: Vec<Candidate> = Vec::with_capacity(remaining.len());
    for index in 0..remaining.len() {
        let Some(lead) = remaining[index].take() else {
            continue;
        };
        let mut group = vec![lead];
        // A sequel can join through the base title, and its collection then brings the rest.
        loop {
            let before = group.len();
            for slot in remaining.iter_mut().skip(index + 1) {
                if slot
                    .as_ref()
                    .is_some_and(|other| group.iter().any(|member| related(member, other)))
                {
                    group.extend(slot.take());
                }
            }
            if group.len() == before {
                break;
            }
        }
        group.sort_by(|a, b| a.date.cmp(&b.date));
        ordered.extend(group);
    }

    ordered
        .into_iter()
        .map(|candidate| {
            let (pick, also_included) = streaming::pick(&candidate.show, country, services);
            let rating = tmdb_id(&candidate.show)
                .and_then(|id| enrichment.signals.get(&id))
                .and_then(|signals| signals.rating.clone());
            Ranked {
                show: candidate.show,
                pick,
                also_included,
                rating,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "relevance-tests.rs"]
pub(crate) mod tests;
