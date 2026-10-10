//! Which country a server is in, from DB-IP's free "IP to Country Lite" database (CC BY 4.0,
//! credited in the settings and the README). It's about 4 MB to download and 8 MB unpacked, comes
//! out monthly, and is kept in the data folder: the browser reads the saved copy at start and
//! downloads a new one in the background once the saved one is a month old. The list works
//! without it; servers just have no country until it's there.

use anyhow::{Context, Result, bail};
use maxminddb::{Reader, geoip2};
use std::io::Read;
use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::http;
use crate::settings::write_atomic;

const FILE: &str = "dbip-country-lite.mmdb";
/// A new database comes out at the start of each month.
const MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// The download is about 4 MB, unpacked about 8 MB.
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;

pub struct Geo {
    reader: Reader<Vec<u8>>,
}

impl Geo {
    /// Reads a database, checking that it's a whole, readable one.
    fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        check(&bytes)?;
        Ok(Self {
            reader: Reader::from_source(bytes).context("not a country database")?,
        })
    }

    /// The country of an IP address, as an upper-case ISO 3166 code such as `DE`.
    pub fn country(&self, ip: &str) -> Option<String> {
        let ip: IpAddr = ip.parse().ok()?;
        let found = self.reader.lookup(ip).ok()?;
        let country = found.decode::<geoip2::Country>().ok()??;
        let code = country.country.iso_code?;
        // DB-IP marks addresses it can't place as `ZZ`.
        (code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic()) && code != "ZZ")
            .then(|| code.to_ascii_uppercase())
    }

    /// When DB-IP made this database.
    pub fn built(&self) -> Option<SystemTime> {
        UNIX_EPOCH.checked_add(Duration::from_secs(self.reader.metadata().build_epoch))
    }
}

/// The saved database, if there's a readable one, and whether it's time to download a new one.
pub fn load(dir: &Path) -> (Option<Geo>, bool) {
    let path = dir.join(FILE);
    let age = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        // A time in the future (the clock moved back) counts as fresh.
        .map(|t| t.elapsed().unwrap_or_default());
    let geo = age.and_then(|_| {
        let bytes = std::fs::read(&path).ok()?;
        Geo::from_bytes(bytes)
            .inspect_err(|e| log::warn!("{}: {e:#}", path.display()))
            .ok()
    });
    let stale = geo.is_none() || age.is_none_or(|age| age > MAX_AGE);
    (geo, stale)
}

/// Downloads this month's database (or last month's, early in a month before the new one is out)
/// and saves it. The saved one stays if anything goes wrong.
pub fn download(dir: &Path) -> Result<Geo> {
    let (year, month) = year_month(SystemTime::now());
    let mut missing = Vec::new();
    for (year, month) in [(year, month), previous_month(year, month)] {
        let url = format!(
            "https://download.db-ip.com/free/dbip-country-lite-{year:04}-{month:02}.mmdb.gz"
        );
        let packed = match http::agent().get(&url).call() {
            Ok(mut response) => response
                .body_mut()
                .with_config()
                .limit(MAX_DOWNLOAD)
                .read_to_vec()
                .context("couldn't download the country database")?,
            Err(ureq::Error::StatusCode(404)) => {
                missing.push(format!("{year:04}-{month:02}"));
                continue;
            }
            Err(e) => return Err(e).context("couldn't reach DB-IP"),
        };
        let bytes = unpack(&packed)?;
        // Checked before it replaces the saved one.
        check(&bytes)?;
        write_atomic(&dir.join(FILE), &bytes)?;
        return Geo::from_bytes(bytes);
    }
    bail!("DB-IP has no country database for {}", missing.join(" or "))
}

/// Checks that a database is a whole, readable country database.
fn check(bytes: &[u8]) -> Result<()> {
    let reader = Reader::from_source(bytes).context("not a country database")?;
    if !reader.metadata().database_type.contains("Country") {
        bail!(
            "not a country database ({})",
            reader.metadata().database_type
        );
    }
    reader.verify().context("the country database is damaged")?;
    Ok(())
}

fn unpack(packed: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(packed)
        .take(MAX_UNPACKED + 1)
        .read_to_end(&mut bytes)
        .context("couldn't unpack the country database")?;
    if bytes.len() as u64 > MAX_UNPACKED {
        bail!("the country database is too big");
    }
    Ok(bytes)
}

/// The year and month (1 to 12) of a time, in UTC, which DB-IP names its files by.
pub fn year_month(time: SystemTime) -> (i32, u32) {
    let days = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400) as i64;
    // Howard Hinnant's days-to-civil algorithm, with years starting in March.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = (year_of_era + era * 400 + i64::from(month <= 2)) as i32;
    (year, month)
}

fn previous_month(year: i32, month: u32) -> (i32, u32) {
    if month <= 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

/// A country's English name, from its ISO 3166 code (either case).
pub fn name(code: &str) -> Option<&'static str> {
    let code = code.to_ascii_uppercase();
    NAMES
        .binary_search_by(|(c, _)| (*c).cmp(code.as_str()))
        .ok()
        .map(|i| NAMES[i].1)
}

/// "Germany (DE)", or just the code for one this table doesn't know.
pub fn describe(code: &str) -> String {
    match name(code) {
        Some(name) => format!("{name} ({code})"),
        None => code.to_string(),
    }
}

/// English country names by ISO 3166-1 code, sorted by code (from the iso-codes project's list,
/// with the common short names: "Russia", not "Russian Federation").
const NAMES: &[(&str, &str)] = &[
    ("AD", "Andorra"),
    ("AE", "United Arab Emirates"),
    ("AF", "Afghanistan"),
    ("AG", "Antigua and Barbuda"),
    ("AI", "Anguilla"),
    ("AL", "Albania"),
    ("AM", "Armenia"),
    ("AO", "Angola"),
    ("AQ", "Antarctica"),
    ("AR", "Argentina"),
    ("AS", "American Samoa"),
    ("AT", "Austria"),
    ("AU", "Australia"),
    ("AW", "Aruba"),
    ("AX", "Åland Islands"),
    ("AZ", "Azerbaijan"),
    ("BA", "Bosnia and Herzegovina"),
    ("BB", "Barbados"),
    ("BD", "Bangladesh"),
    ("BE", "Belgium"),
    ("BF", "Burkina Faso"),
    ("BG", "Bulgaria"),
    ("BH", "Bahrain"),
    ("BI", "Burundi"),
    ("BJ", "Benin"),
    ("BL", "Saint Barthélemy"),
    ("BM", "Bermuda"),
    ("BN", "Brunei"),
    ("BO", "Bolivia"),
    ("BQ", "Caribbean Netherlands"),
    ("BR", "Brazil"),
    ("BS", "Bahamas"),
    ("BT", "Bhutan"),
    ("BV", "Bouvet Island"),
    ("BW", "Botswana"),
    ("BY", "Belarus"),
    ("BZ", "Belize"),
    ("CA", "Canada"),
    ("CC", "Cocos (Keeling) Islands"),
    ("CD", "DR Congo"),
    ("CF", "Central African Republic"),
    ("CG", "Congo"),
    ("CH", "Switzerland"),
    ("CI", "Côte d'Ivoire"),
    ("CK", "Cook Islands"),
    ("CL", "Chile"),
    ("CM", "Cameroon"),
    ("CN", "China"),
    ("CO", "Colombia"),
    ("CR", "Costa Rica"),
    ("CU", "Cuba"),
    ("CV", "Cabo Verde"),
    ("CW", "Curaçao"),
    ("CX", "Christmas Island"),
    ("CY", "Cyprus"),
    ("CZ", "Czechia"),
    ("DE", "Germany"),
    ("DJ", "Djibouti"),
    ("DK", "Denmark"),
    ("DM", "Dominica"),
    ("DO", "Dominican Republic"),
    ("DZ", "Algeria"),
    ("EC", "Ecuador"),
    ("EE", "Estonia"),
    ("EG", "Egypt"),
    ("EH", "Western Sahara"),
    ("ER", "Eritrea"),
    ("ES", "Spain"),
    ("ET", "Ethiopia"),
    ("FI", "Finland"),
    ("FJ", "Fiji"),
    ("FK", "Falkland Islands"),
    ("FM", "Micronesia"),
    ("FO", "Faroe Islands"),
    ("FR", "France"),
    ("GA", "Gabon"),
    ("GB", "United Kingdom"),
    ("GD", "Grenada"),
    ("GE", "Georgia"),
    ("GF", "French Guiana"),
    ("GG", "Guernsey"),
    ("GH", "Ghana"),
    ("GI", "Gibraltar"),
    ("GL", "Greenland"),
    ("GM", "Gambia"),
    ("GN", "Guinea"),
    ("GP", "Guadeloupe"),
    ("GQ", "Equatorial Guinea"),
    ("GR", "Greece"),
    ("GS", "South Georgia and the South Sandwich Islands"),
    ("GT", "Guatemala"),
    ("GU", "Guam"),
    ("GW", "Guinea-Bissau"),
    ("GY", "Guyana"),
    ("HK", "Hong Kong"),
    ("HM", "Heard and McDonald Islands"),
    ("HN", "Honduras"),
    ("HR", "Croatia"),
    ("HT", "Haiti"),
    ("HU", "Hungary"),
    ("ID", "Indonesia"),
    ("IE", "Ireland"),
    ("IL", "Israel"),
    ("IM", "Isle of Man"),
    ("IN", "India"),
    ("IO", "British Indian Ocean Territory"),
    ("IQ", "Iraq"),
    ("IR", "Iran"),
    ("IS", "Iceland"),
    ("IT", "Italy"),
    ("JE", "Jersey"),
    ("JM", "Jamaica"),
    ("JO", "Jordan"),
    ("JP", "Japan"),
    ("KE", "Kenya"),
    ("KG", "Kyrgyzstan"),
    ("KH", "Cambodia"),
    ("KI", "Kiribati"),
    ("KM", "Comoros"),
    ("KN", "Saint Kitts and Nevis"),
    ("KP", "North Korea"),
    ("KR", "South Korea"),
    ("KW", "Kuwait"),
    ("KY", "Cayman Islands"),
    ("KZ", "Kazakhstan"),
    ("LA", "Laos"),
    ("LB", "Lebanon"),
    ("LC", "Saint Lucia"),
    ("LI", "Liechtenstein"),
    ("LK", "Sri Lanka"),
    ("LR", "Liberia"),
    ("LS", "Lesotho"),
    ("LT", "Lithuania"),
    ("LU", "Luxembourg"),
    ("LV", "Latvia"),
    ("LY", "Libya"),
    ("MA", "Morocco"),
    ("MC", "Monaco"),
    ("MD", "Moldova"),
    ("ME", "Montenegro"),
    ("MF", "Saint Martin"),
    ("MG", "Madagascar"),
    ("MH", "Marshall Islands"),
    ("MK", "North Macedonia"),
    ("ML", "Mali"),
    ("MM", "Myanmar"),
    ("MN", "Mongolia"),
    ("MO", "Macao"),
    ("MP", "Northern Mariana Islands"),
    ("MQ", "Martinique"),
    ("MR", "Mauritania"),
    ("MS", "Montserrat"),
    ("MT", "Malta"),
    ("MU", "Mauritius"),
    ("MV", "Maldives"),
    ("MW", "Malawi"),
    ("MX", "Mexico"),
    ("MY", "Malaysia"),
    ("MZ", "Mozambique"),
    ("NA", "Namibia"),
    ("NC", "New Caledonia"),
    ("NE", "Niger"),
    ("NF", "Norfolk Island"),
    ("NG", "Nigeria"),
    ("NI", "Nicaragua"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("NP", "Nepal"),
    ("NR", "Nauru"),
    ("NU", "Niue"),
    ("NZ", "New Zealand"),
    ("OM", "Oman"),
    ("PA", "Panama"),
    ("PE", "Peru"),
    ("PF", "French Polynesia"),
    ("PG", "Papua New Guinea"),
    ("PH", "Philippines"),
    ("PK", "Pakistan"),
    ("PL", "Poland"),
    ("PM", "Saint Pierre and Miquelon"),
    ("PN", "Pitcairn"),
    ("PR", "Puerto Rico"),
    ("PS", "Palestine"),
    ("PT", "Portugal"),
    ("PW", "Palau"),
    ("PY", "Paraguay"),
    ("QA", "Qatar"),
    ("RE", "Réunion"),
    ("RO", "Romania"),
    ("RS", "Serbia"),
    ("RU", "Russia"),
    ("RW", "Rwanda"),
    ("SA", "Saudi Arabia"),
    ("SB", "Solomon Islands"),
    ("SC", "Seychelles"),
    ("SD", "Sudan"),
    ("SE", "Sweden"),
    ("SG", "Singapore"),
    ("SH", "Saint Helena"),
    ("SI", "Slovenia"),
    ("SJ", "Svalbard and Jan Mayen"),
    ("SK", "Slovakia"),
    ("SL", "Sierra Leone"),
    ("SM", "San Marino"),
    ("SN", "Senegal"),
    ("SO", "Somalia"),
    ("SR", "Suriname"),
    ("SS", "South Sudan"),
    ("ST", "Sao Tome and Principe"),
    ("SV", "El Salvador"),
    ("SX", "Sint Maarten"),
    ("SY", "Syria"),
    ("SZ", "Eswatini"),
    ("TC", "Turks and Caicos Islands"),
    ("TD", "Chad"),
    ("TF", "French Southern Territories"),
    ("TG", "Togo"),
    ("TH", "Thailand"),
    ("TJ", "Tajikistan"),
    ("TK", "Tokelau"),
    ("TL", "Timor-Leste"),
    ("TM", "Turkmenistan"),
    ("TN", "Tunisia"),
    ("TO", "Tonga"),
    ("TR", "Türkiye"),
    ("TT", "Trinidad and Tobago"),
    ("TV", "Tuvalu"),
    ("TW", "Taiwan"),
    ("TZ", "Tanzania"),
    ("UA", "Ukraine"),
    ("UG", "Uganda"),
    ("UM", "U.S. Outlying Islands"),
    ("US", "United States"),
    ("UY", "Uruguay"),
    ("UZ", "Uzbekistan"),
    ("VA", "Vatican City"),
    ("VC", "Saint Vincent and the Grenadines"),
    ("VE", "Venezuela"),
    ("VG", "British Virgin Islands"),
    ("VI", "U.S. Virgin Islands"),
    ("VN", "Vietnam"),
    ("VU", "Vanuatu"),
    ("WF", "Wallis and Futuna"),
    ("WS", "Samoa"),
    ("YE", "Yemen"),
    ("YT", "Mayotte"),
    ("ZA", "South Africa"),
    ("ZM", "Zambia"),
    ("ZW", "Zimbabwe"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months() {
        let at = |secs: u64| year_month(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(at(0), (1970, 1));
        // 2026-10-10 and the last second of 2026-09-30, both UTC.
        assert_eq!(at(1_791_590_400), (2026, 10));
        assert_eq!(at(1_790_812_799), (2026, 9));
        // 2024-02-29 (a leap day) and 2000-03-01.
        assert_eq!(at(1_709_164_800), (2024, 2));
        assert_eq!(at(951_868_800), (2000, 3));
        assert_eq!(previous_month(2026, 1), (2025, 12));
        assert_eq!(previous_month(2026, 10), (2026, 9));
    }

    #[test]
    fn names() {
        assert!(
            NAMES.windows(2).all(|w| w[0].0 < w[1].0),
            "sorted for searching"
        );
        assert_eq!(name("de"), Some("Germany"));
        assert_eq!(name("US"), Some("United States"));
        assert_eq!(name("ZZ"), None);
        assert_eq!(describe("GB"), "United Kingdom (GB)");
        assert_eq!(describe("XK"), "XK");
    }

    #[test]
    fn rejects_bad_downloads() {
        assert!(unpack(b"not gzip").is_err());
        assert!(Geo::from_bytes(b"not a database".to_vec()).is_err());
        let dir = std::env::temp_dir().join(format!("dzb-geo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(matches!(load(&dir), (None, true)), "nothing saved yet");
        std::fs::write(dir.join(FILE), b"junk").unwrap();
        assert!(
            matches!(load(&dir), (None, true)),
            "unreadable: download again"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Reads a real database: `DZB_GEO_DB=<path to dbip-country-lite.mmdb> cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_database() {
        let path = std::env::var("DZB_GEO_DB").expect("DZB_GEO_DB");
        let geo = Geo::from_bytes(std::fs::read(path).unwrap()).unwrap();
        assert_eq!(geo.country("1.1.1.1").as_deref(), Some("AU"));
        assert_eq!(geo.country("8.8.8.8").as_deref(), Some("US"));
        assert_eq!(geo.country("not an ip"), None);
        assert!(geo.built().is_some());
    }
}
