/**
 * The build profile the web asks for, previews and reads the status of
 * (decision 0058, rule 14): the only profile the record defines. A leaf module,
 * so the Jobs tab's rules and words and the preview's build choice share it
 * without importing each other.
 */
export const BUILD_PROFILE = 'react-v1';

/**
 * What the web says while the server cannot build the profile: the Jobs tab's
 * banner, a refused "Build revision N", and the preview's build choice.
 */
export const PROFILE_UNKNOWN_TEXT = `This server cannot build the ${BUILD_PROFILE} profile (profile_unknown).`;
