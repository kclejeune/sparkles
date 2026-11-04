import { describe, expect, it } from 'vitest';
import { pickMove } from './balance';

const card = (height: number, inMain = true, locked = false) => ({ height, inMain, locked });

describe('pickMove', () => {
  it('moves the first card whose move brings the columns closer', () => {
    // main 1600, side 600: References (500) to the side leaves 1100 against 1100
    expect(pickMove(1600, 600, [card(500), card(200, false)], 0, 64)).toBe(0);
  });
  it('leaves the cards when a move would not help by more than the threshold', () => {
    expect(pickMove(1000, 900, [card(500), card(200, false)], 0, 64)).toBe(-1);
    // a gap of 100 and a card of 40: 100 to 20 is a gain of 80, past 64
    expect(pickMove(1000, 900, [card(40)], 0, 64)).toBe(0);
    expect(pickMove(1000, 950, [card(20)], 0, 64)).toBe(-1);
  });
  it('never moves a card that holds focus', () => {
    expect(pickMove(1600, 600, [card(500, true, true), card(300)], 0, 64)).toBe(1);
  });
  it('moves a card home when the other column grew', () => {
    // References sits in the side column, which is now far longer
    expect(pickMove(400, 1400, [card(500, false)], 0, 64)).toBe(0);
  });
  it('counts the gap between cards', () => {
    expect(pickMove(1000, 700, [card(100)], 16, 64)).toBe(0);
    expect(pickMove(1000, 900, [card(100)], 16, 64)).toBe(-1);
  });
});
