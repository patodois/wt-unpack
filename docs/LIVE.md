# Live telemetry (official localhost:8111 API)

`wt-unpack live` and `src/live.rs`. Part of the [wt-unpack README](../README.md).

While a match or test drive runs, the War Thunder client itself serves read-only JSON on
`http://localhost:8111`, the same data its in-game browser pages and community overlays use.
`wt-unpack live` polls it and prints one line about the player's vehicle; `src/live.rs` is the library
side, for feeding the live state into a scene.

It only reads what the game publishes on that port: the player's own gauges and the objects already drawn
on the player's minimap. It never reads the game's memory, injects anything or modifies the client, and
it adds no markers or information the player does not already have.

| Endpoint | Used for |
|---|---|
| `/indicators` | Vehicle kind (`army`) and unit (`type`), speed, gearbox (`gear`, `gear_neutral`), RPM, crew; throttle and compass for aircraft |
| `/state` | Aircraft flight model: altitude, true airspeed, throttle, landing gear (always `{"valid": false}` for ground vehicles) |
| `/map_info.json` | The minimap's bounds in metres (`map_min`, `map_max`), read once per match |
| `/map_obj.json` | The player's icon (`"icon": "Player"`): position and facing on the minimap |
| `/map.img` | The minimap image (library only) |

Positions come out in metres in the crate's glTF convention, +Y up and right-handed, with map north on
+Z, so +X points west:

```text
east  = map_min[0] + x * (map_max[0] - map_min[0])       # x, y: map_obj position, 0..1, y downwards
north = map_max[1] - y * (map_max[1] - map_min[1])
[X, Y, Z] = [-east, altitude, north]                     # altitude: "H, m" for aircraft, else 0
```

Headings are compass degrees, clockwise from north; heading `h` points along `[-sin h, 0, cos h]`. The
module docs in `src/live.rs` have the details. Every field the game sends is kept as raw JSON next to the
typed ones, so new or renamed fields survive a game update.

```sh
cargo build --release --features live

# Start War Thunder, open a test drive (or a battle), then:
wt-unpack live                        # one line per poll, 5 per second
wt-unpack live --hz 10 --log run.jsonl # also append one JSON line per poll to a file

# Without the game: a built-in mock server with a tank driving in a circle
wt-unpack live --mock --count 20
```

A line looks like `tank us_m4a1_76w_sherman  23.4 km/h  hdg 037  pos -900.0 0.0 1300.0 m  thr -  gear 3`.
Gears show as `R`, `N` and `1`..`n`; speed is negative in reverse, and tanks report no throttle. With the
game closed it says it is waiting; in the hangar it says the game is not in a match, and it polls once a
second until a match starts. The JSON log holds the combined values, the raw `/state` and `/indicators`,
the player's own minimap entry, and `map_info` on the first poll of each match.

About 2% of polls hiccup: `/map_obj.json` answers with an empty body, or a request times out. In a match
the poller bridges these for up to 2 seconds with the last good answer (the log lists them under `held`);
an empty body is logged as a note, not an error. A connection that cannot be made ends the match at
once, as does `{"valid": false}`. On Windows a closed port is not refused straight away (the OS retries
for about 2 seconds), so connecting has its own 250 ms timeout, separate from the request's 500 ms.

A test drive with a T-26 confirmed the units (speed in km/h, positions in metres: the speed derived from
the positions matches the speedometer within about 1 km/h), the heading, and the gear numbering. Not
yet checked: whether map north is +Z of the level geometry exported from the game files.

The field names and units come from open-source community clients, chiefly the
[WarThunder](https://github.com/PowerBroker2/WarThunder) Python package, and the samples the tests serve
(`samples/live/`) are written by hand in that shape. They are not recordings of the game.
