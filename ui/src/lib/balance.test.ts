import { describe, expect, it } from 'vitest';
import { pickMove, stickTop } from './balance';

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

describe('stickTop', () => {
  it('is the gutter for a column that fits in the box', () => {
    expect(stickTop(24, 900, 400)).toBe(24);
    // exactly a gutter of room above and below
    expect(stickTop(24, 900, 852)).toBe(24);
  });
  it('holds a taller column by its bottom edge, a gutter above the box bottom', () => {
    expect(stickTop(24, 900, 860)).toBe(16);
    expect(stickTop(24, 900, 1500)).toBe(-624);
    expect(stickTop(16, 844, 2000)).toBe(-1172);
  });
});
