//! Which app an installer or an update works on.

/// A Craft App, as its releases and installers know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct App {
    /// Machine name: repo, binary, package and asset stem (`photocraft`).
    pub id: &'static str,
    /// Display name (`PhotoCraft`).
    pub name: &'static str,
    /// What it's for, in a few words (English). Written into the menu entries installers add.
    pub tagline: &'static str,
    /// GitHub `owner/repo`. Releases are only ever downloaded from this repository.
    pub repo: &'static str,
    /// Older names its release assets may carry (PdfCraft shipped as PrintCraft until 0.2.1).
    pub former_ids: &'static [&'static str],
    /// Older display names its installers registered (PdfCraft's MSI said PrintCraft until 0.2.1).
    pub former_names: &'static [&'static str],
    /// Its 128 px PNG icon, for the menu entry written beside an AppImage. `None`: no icon.
    pub icon_png: Option<&'static [u8]>,
    /// Its accent colour (sRGB), for the update prompt's buttons and highlights: the app's brand
    /// colour, as ArtCraft Launcher shows it. `None`: the default blue.
    pub accent: Option<[u8; 3]>,
    /// Who makes it, shown as "by <developer>" in the update prompt. [`DEVELOPER`] unless the app
    /// says otherwise.
    pub developer: &'static str,
    /// Its own reverse-DNS identifier, for an app outside the suite's `ai.storyteller.` namespace
    /// (Filmagic: `io.github.vansyson1308.editfilm`). `None`: `ai.storyteller.<id>`. Filmagic change.
    pub bundle_id_override: Option<&'static str>,
    /// The publisher its Windows Installer package registers (the uninstall entry's `Publisher`).
    /// [`SUITE_PUBLISHER`] unless the app says otherwise. Filmagic change.
    pub publisher: &'static str,
    /// The registry key, under `HKCU` or `HKLM`, where its Windows installer records the install
    /// folder (`InstallDir`). `None`: `Software\<publisher>\<name>`, the suite's layout. Filmagic change.
    pub install_key_override: Option<&'static str>,
    /// The Apple Developer team whose Developer ID signature a macOS update must carry.
    /// [`SUITE_APPLE_TEAM`] unless the app says otherwise. `None`: the app has no team yet, so
    /// macOS updates are refused rather than installed unverified ([`App::apple_team`]). Filmagic change.
    pub apple_team: Option<&'static str>,
}

/// The developer of every app in the storytold organisation.
pub const DEVELOPER: &str = "ArtCraft";

/// The publisher of the suite's Windows Installer packages.
pub const SUITE_PUBLISHER: &str = "Learning Machines LLC";

/// The Apple Developer team that signs the suite's apps (Learning Machines LLC).
pub const SUITE_APPLE_TEAM: &str = "DJ6XS33FX8";

impl App {
    /// An app with no former names and no icon.
    pub const fn new(id: &'static str, name: &'static str, tagline: &'static str, repo: &'static str) -> Self {
        Self {
            id,
            name,
            tagline,
            repo,
            former_ids: &[],
            former_names: &[],
            icon_png: None,
            accent: None,
            developer: DEVELOPER,
            bundle_id_override: None,
            publisher: SUITE_PUBLISHER,
            install_key_override: None,
            apple_team: Some(SUITE_APPLE_TEAM),
        }
    }

    /// The registry key (under `HKCU` or `HKLM`) where the Windows installer records the install
    /// folder: [`App::install_key_override`], else `Software\<publisher>\<name>`.
    pub fn install_key(&self) -> String {
        match self.install_key_override {
            Some(key) => key.to_string(),
            None => format!("Software\\{}\\{}", self.publisher, self.name),
        }
    }

    /// The Apple Developer team a macOS update must be signed by, or why the app can't update
    /// itself on macOS: without a team there is no signature to check, and an unverified copy is
    /// never installed.
    pub fn apple_team(&self) -> Result<&'static str, String> {
        self.apple_team.ok_or_else(|| {
            format!(
                "{} isn't signed with an Apple Developer ID yet, so it can't check that an update is genuine and doesn't install it. Download the new version from {}",
                self.name,
                self.releases_url()
            )
        })
    }

    /// The macOS bundle id, Flatpak id and Linux desktop-file id: [`App::bundle_id_override`] when
    /// the app names one, else `ai.storyteller.<id>`.
    pub fn bundle_id(&self) -> String {
        match self.bundle_id_override {
            Some(id) => id.to_string(),
            None => format!("ai.storyteller.{}", self.id),
        }
    }

    pub fn releases_url(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// Asset stems this app's releases may use, current first.
    pub fn stems(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.id).chain(self.former_ids.iter().copied())
    }

    /// Display names this app's installers may have registered, current first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.name).chain(self.former_names.iter().copied())
    }

    /// Looks up an app in the Craft App Suite catalog by its id.
    pub fn by_id(id: &str) -> Option<&'static App> {
        crate::suite::by_id(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn former_names_follow_the_current_one() {
        let pdf =
            App { former_ids: &["printcraft"], former_names: &["PrintCraft"], ..App::new("pdfcraft", "PdfCraft", "PDFs", "storytold/pdfcraft") };
        assert_eq!(pdf.stems().collect::<Vec<_>>(), ["pdfcraft", "printcraft"]);
        assert_eq!(pdf.names().collect::<Vec<_>>(), ["PdfCraft", "PrintCraft"]);
        assert_eq!(pdf.bundle_id(), "ai.storyteller.pdfcraft");
        assert_eq!(pdf.releases_url(), "https://github.com/storytold/pdfcraft/releases");
    }

    #[test]
    fn suite_apps_keep_the_suite_publisher_key_and_team() {
        let pdf = App::new("pdfcraft", "PdfCraft", "PDFs", "storytold/pdfcraft");
        assert_eq!(pdf.publisher, SUITE_PUBLISHER);
        assert_eq!(pdf.install_key(), r"Software\Learning Machines LLC\PdfCraft");
        assert_eq!(pdf.apple_team(), Ok(SUITE_APPLE_TEAM));
    }

    #[test]
    fn an_app_can_name_its_own_publisher_key_and_team() {
        let own = App {
            publisher: "Example contributors",
            install_key_override: Some(r"Software\Editor"),
            apple_team: Some("ABCDE12345"),
            ..App::new("editor", "Editor", "Editing", "example/editor")
        };
        assert_eq!(own.install_key(), r"Software\Editor");
        assert_eq!(own.apple_team(), Ok("ABCDE12345"));
    }

    #[test]
    fn an_app_without_an_apple_team_refuses_macos_updates_and_says_where_to_download() {
        let own = App { apple_team: None, ..App::new("editor", "Editor", "Editing", "example/editor") };
        let Err(why) = own.apple_team() else { panic!("an app without a team must not verify") };
        assert!(why.contains("Editor isn't signed with an Apple Developer ID"), "{why}");
        assert!(why.contains("https://github.com/example/editor/releases"), "{why}");
    }

    #[test]
    fn an_app_can_name_its_own_bundle_id() {
        let own = App { bundle_id_override: Some("io.github.example.editor"), ..App::new("editor", "Editor", "Editing", "example/editor") };
        assert_eq!(own.bundle_id(), "io.github.example.editor");
        assert_eq!(own.stems().collect::<Vec<_>>(), ["editor"], "the asset stem stays the machine name");
    }
}
