# RHP Wishing Well Family (10)

Status: **Wave 3.2** — a request board for wanted files, boards, and
features. Members post wishes, vote to surface the popular ones, and
privileged users claim and fulfill them.

| type | name | direction | payload |
|---|---|---|---|
| 1/2 | WishListRequest → WishList | Request/Reply | `status` (None = all), `limit`; views ordered most-voted then newest |
| 3/6 | WishCreate → WishReply | Request/Reply | `kind` (0 file/1 board/2 feature/3 other), `title` (≤200), `details`; members only |
| 4/6 | WishVote → WishReply | Request/Reply | toggle the caller's vote; returns updated counts |
| 5/6 | WishSetStatus → WishReply | Request/Reply | `id`, `status`, optional `fulfillment` note |
| 7 | WishUpdated | Push | a wish **you requested** changed status |

## WishView

The projected wish on the wire: `id`, `kind`, `title`, `details`,
`requester` (`persona@origin`), `status` (0 open / 1 claimed / 2 fulfilled
/ 3 declined), `claimed_by`, `fulfillment`, `votes`, `created_at_unix`.

## Status transitions & authorization

Status changes are gated per transition (checked in `handlers7`):

| target | who may set it |
|---|---|
| **claimed** (1) | anyone with `FILE_UPLOAD` (fulfillers), or `BOARD_MODERATE` |
| **fulfilled** (2) | the claimer, a moderator, or any `FILE_UPLOAD` holder |
| **declined** (3) | the requester (withdraw), or a moderator |
| **open** (0) | the requester (reopen), or a moderator |

Claiming stamps `claimed_by = persona@origin`; the claim is preserved
through a later fulfill (the store `COALESCE`s it). Guests may browse the
list but cannot create, vote, or change status.

## Notifications

When someone **other than the requester** changes a wish, the server
publishes `ServerEvent::WishUpdated { to_account, wish }` on the bus. Push
projection is synchronous (the full `WishView` rides the event, so there is
no DB round-trip), filtered to the requester's account — and it rides the
offline replay ring like any other targeted push, so a requester who was
away learns of a claim/fulfillment on their next resume.

## Storage

`wishes` + `wish_votes` (migration 0006). Votes are counted via a
correlated subquery; `wish_votes` has a `(wish_id, account_id)` primary key
so a vote is idempotent and toggling deletes the row. Listing orders by
vote count then `updated_at`.
