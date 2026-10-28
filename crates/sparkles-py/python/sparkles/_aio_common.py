"""Native token, deadline and cleanup helpers for the async facade."""

from __future__ import annotations

import asyncio
import math
import time
from typing import Any, TypeVar

from ._aio_worker import _owned_wait, _settle
from ._errors import InvalidInputError, QueryTimeoutError
from ._sparkles import CancelToken

_T = TypeVar("_T")


def _token(caller: CancelToken | None = None) -> CancelToken:
    token = CancelToken()
    return token if caller is None else token._with_cancel(caller)


def _deadline(options: dict[str, Any], parent: float | None = None) -> float | None:
    seconds = options.get("timeout")
    if seconds is None:
        return parent
    seconds = float(seconds)
    if not math.isfinite(seconds) or seconds < 0:
        raise InvalidInputError("timeout must be finite and nonnegative")
    deadline = time.monotonic() + seconds
    if not math.isfinite(deadline):
        raise InvalidInputError("timeout is too large")
    return deadline if parent is None else min(deadline, parent)


def _remaining(deadline: float | None) -> float | None:
    if deadline is None:
        return None
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise QueryTimeoutError("operation timed out before native dispatch")
    return remaining


def _options(
    options: dict[str, Any], token: CancelToken, deadline: float | None
) -> dict[str, Any]:
    result = dict(options, cancel=token)
    if deadline is not None:
        result["timeout"] = _remaining(deadline)
    return result


async def _finish(task: asyncio.Future[_T]) -> _T:
    """Explicit close keeps owning its cleanup even if its waiter is cancelled."""
    try:
        return await _owned_wait(task)
    except asyncio.CancelledError as cancelled:
        try:
            await _settle(task, lambda: None)
        except BaseException as error:
            raise cancelled from error
        raise
