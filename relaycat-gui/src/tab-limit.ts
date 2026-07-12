export const MAX_TAB_COUNT = 24;

export function canCreateTab(tabCount: number): boolean {
  return tabCount < MAX_TAB_COUNT;
}
