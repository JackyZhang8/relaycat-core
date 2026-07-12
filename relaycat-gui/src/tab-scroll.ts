export interface TabScrollMetrics {
  scrollLeft: number;
  clientWidth: number;
  scrollWidth: number;
}

export interface TabScrollState {
  overflow: boolean;
  canScrollLeft: boolean;
  canScrollRight: boolean;
}

const EDGE_TOLERANCE = 1;

export function tabScrollState({
  scrollLeft,
  clientWidth,
  scrollWidth,
}: TabScrollMetrics): TabScrollState {
  const overflow = scrollWidth > clientWidth + EDGE_TOLERANCE;
  if (!overflow) {
    return { overflow: false, canScrollLeft: false, canScrollRight: false };
  }

  return {
    overflow: true,
    canScrollLeft: scrollLeft > EDGE_TOLERANCE,
    canScrollRight: scrollLeft + clientWidth < scrollWidth - EDGE_TOLERANCE,
  };
}
