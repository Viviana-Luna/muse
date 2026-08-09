export type AppearanceTheme = 'system' | 'dark' | 'light';

export type ResolvedAppearanceTheme = Exclude<AppearanceTheme, 'system'>;

export type AppearanceBackgroundTheme = 'dark' | 'light';
