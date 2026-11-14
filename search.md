# Movies & shows search: goals

What the Search tab should do and how we judge it. How it is built is in
[development.md](development.md) (the Search tab paragraphs); the code is `src/streaming-search.rs` (calls),
`src/relevance.rs` (filtering and ranking) and `src/deeplink.rs` (opening a title).

## Who it is for

One household watching commercial work, mostly from US studios and networks, on the apps
installed on its LG TV. Typing should feel like a search box that knows what you mean: the
title you are thinking of comes first, often before you finish typing it.

## Goals

1. **Every result says what you typed.** Its title, original title or an English (US or UK)
   alternative title contains the query, or it belongs to the same film collection as one that
   does. Movie of the Night pads its 20 results with titles matched in other languages; those
   go. _Andor_ stays for "star wars" because its US title is "Star Wars: Andor".
2. **The title you mean comes first.** Exact and leading-word matches, then prefixes, then whole
   words, then anywhere. Within a group: major US studio or network, US production, how widely
   seen, budget and box office.
3. **Franchises stay together, oldest first.** "ghostbusters" lists the five films from 1984,
   including sequels the title search missed (looked up through the TMDB collection).
4. **Foreign and obscure titles stay out**, unless the query names them: "amélie" finds
   _Amélie_. English-language work from abroad stays, ranked lower (BBC _Ghosts_).
5. **Punctuation and spacing do not matter.** "oceans" finds _Ocean's Eleven_, "spiderman" finds
   _Spider-Man_, "super man" finds _Superman_.
6. **Type-ahead.** A partial word should find titles that start with it: "sup" → _Superman_,
   _Supergirl_, _Superbad_; "fan" → _Fantasia_, _Fantastic Four_. **Not yet met**: see below.
7. **OK opens the title itself**, not the app's home screen, in an app the TV has. Where it
   streams is a tiebreaker for ranking, never a reason to rank an unrelated title higher.
8. **Cheap.** Repeating a search costs nothing (disk cache). Movie of the Night calls are
   metered, so extra lookups are capped; TMDB calls are free but still cached.

## Example searches

Each is a test against captured replies in `tests/fixtures` (`pnpm fixtures:streaming` adds
more without re-fetching the rest).

| Query        | Expect                                                                    | Test                                                        |
| ------------ | ------------------------------------------------------------------------- | ----------------------------------------------------------- |
| ghostbusters | Five US films oldest first, then the cartoons                             | `ghostbusters_lists_the_five_us_films_oldest_first_…`       |
| star wars    | _Andor_, _The Mandalorian_, _Ahsoka_, _Return of the Jedi_ kept           | `star_wars_keeps_the_franchise_titles_that_do_not_say_…`    |
| super        | _Superman_, _Super 8_, _Superbad_; not _8MM_, _The Bad Guys 2_, _Argylle_ | `super_drops_titles_that_do_not_say_super`                  |
| fan          | Only titles with "fan" in them; not _One Piece_, _Rumble in the Bronx_    | `fan_lists_only_titles_with_fan_in_them`                    |
| gho          | _Ghosts_ (US) above _Ghosts_ (UK); no Marathi or Kannada titles           | `english_language_work_from_abroad_stays_but_ranks_below_…` |
| amélie       | _Amélie_ first, though it is French                                       | `foreign_language_titles_from_abroad_are_filtered_out_…`    |
| sup          | Nothing matches yet: keep the near misses rather than show nothing        | `when_nothing_matches_the_near_misses_stay`                 |

## Opening a title (deep links)

| App         | Status                                                                                               |
| ----------- | ---------------------------------------------------------------------------------------------------- |
| Apple TV    | Confirmed on a TV: the web link as `contentTarget`                                                   |
| Prime Video | Confirmed on a TV: the title's GTI (`amzn1.dv.gti.<uuid>`) as `contentTarget`, decoded from the link |
| Others      | Best-known formats, not yet confirmed; check with `pnpm deeplink:probe`                              |

To learn an app's format, watch the TV's own search launch it: see "Reaching the development
TV" in [agents.md](agents.md).

## Known gaps and next steps

- **Type-ahead (goal 6).** Neither Movie of the Night nor TMDB searches by prefix: "sup" never
  returns _Superman_, and "super" misses _Supergirl_. Plan: build a local title index from TMDB's
  free daily ID exports (titles with popularity), complete the typed prefix against it, and look
  up the top few completions in Movie of the Night (metered, so capped and cached).
- **Deep links** for Netflix, Disney+, Max, Hulu, Peacock and Paramount+ are unconfirmed.
- Prime links with older ASIN-style IDs (`B0FV8FQK6M`) cannot be converted and open the app.

## Decisions

- **2026-11:** Dropping results by their own title alone hid _Andor_ and _The Mandalorian_ for
  "star wars". English alternative titles (appended to the TMDB details call, no extra request)
  tell franchise entries from near misses; keywords and production companies did not.
- **2026-11:** Movie of the Night's title search has no language filter. Its filter search has
  one, but matches whole words in titles _and_ plot summaries, so we filter on our side.
