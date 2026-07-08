# Transit

A tiny transport network in one HTML file. No build, no dependencies —
open `index.html` in a browser (or drop the folder behind any static file
server) and play.

Towns appear on a procedurally generated coastline and start wanting
things: **people are blue dots, freight is red squares**. Drag between two
towns to lay infrastructure, and vehicles start shuttling. Passengers and
parcels route themselves across your whole network — transfers included —
pay their fare in gold on arrival, and give up if you leave them waiting
too long. Served towns grow, and bigger towns demand more.

## Modes

| tool | vehicles | carries | niche |
|---|---|---|---|
| **Road** | taxi, truck | people / freight | cheap, everywhere |
| **Rail** | train | both, high capacity | busy trunk lines |
| **Subway** | metro | people | short hops, tunnels under water |
| **Air** | plane | people + a little freight | long distance, very fast |
| **Ship** | ship | freight-heavy | coastal towns with a clear sea lane |

## Controls

- **Drag** town → town to build with the selected tool
- **Click a line** to add vehicles or tear it up (half refund)
- **Hover a town** to see where its waiting demand wants to go
- **1–5** switch tools · **space** pauses · **▶▶** runs at 3×
- Progress autosaves locally; **NEW MAP** rolls a fresh world

## Design notes

Deliberately minimal: paper, ink, and grey for the world; colour is spent
only where it means something — red freight, blue people, gold money.
Vehicles are white with ink outlines so their cargo shows through.
