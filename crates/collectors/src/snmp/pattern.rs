//! Filtrage par motif, pour les profils YAML.
//!
//! Le crate `regex` ne fait pas partie des dépendances du serveur et l'ajouter pour
//! quelques motifs de filtrage — « ^lo$ », « ^veth », « Linux.* » — coûterait plus
//! d'un mégaoctet de binaire. On implémente donc le sous-ensemble réellement utile :
//! ancres `^` et `$`, `.`, classes `[abc]` `[^abc]` `[a-z]`, quantificateurs `*` `+`
//! `?`, alternation `|` de premier niveau et échappement par `\`.
//!
//! Les groupes `( )` et les quantificateurs bornés `{n,m}` ne sont volontairement pas
//! pris en charge : un motif qui en contient est rejeté à l'analyse du profil, ce qui
//! vaut mieux qu'un filtre silencieusement inopérant.

use std::fmt;

/// Un élément du motif, avec son quantificateur.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Piece {
    atom: Atom,
    repeat: Repeat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    Literal(char),
    Any,
    Class { negated: bool, ranges: Vec<(char, char)> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repeat {
    One,
    ZeroOrMore,
    OneOrMore,
    ZeroOrOne,
}

/// Une des branches d'une alternation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Branch {
    anchored_start: bool,
    anchored_end: bool,
    pieces: Vec<Piece>,
}

/// Un motif compilé, réutilisable sans allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    source: String,
    branches: Vec<Branch>,
    /// `*` seul : accepte tout. C'est le repli universel des profils.
    match_all: bool,
}

impl Pattern {
    pub fn compile(source: &str) -> Result<Self, String> {
        if source == "*" {
            return Ok(Self { source: source.to_string(), branches: Vec::new(), match_all: true });
        }
        let mut branches = Vec::new();
        for raw in split_alternation(source)? {
            branches.push(compile_branch(&raw)?);
        }
        Ok(Self { source: source.to_string(), branches, match_all: false })
    }

    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Vrai si le motif est le repli universel `*`.
    pub fn is_wildcard(&self) -> bool {
        self.match_all
    }

    /// Recherche non ancrée, comme `Regex::is_match` : le motif peut correspondre
    /// n'importe où dans le texte, sauf ancrage explicite.
    pub fn is_match(&self, text: &str) -> bool {
        if self.match_all {
            return true;
        }
        let chars: Vec<char> = text.chars().collect();
        self.branches.iter().any(|branch| branch_matches(branch, &chars))
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

impl<'de> serde::Deserialize<'de> for Pattern {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Pattern::compile(&raw).map_err(serde::de::Error::custom)
    }
}

/// Découpe sur les `|` non échappés et hors classe de caractères.
fn split_alternation(source: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_class = false;
    let mut escaped = false;

    for character in source.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' => {
                current.push(character);
                escaped = true;
            }
            '[' => {
                in_class = true;
                current.push(character);
            }
            ']' => {
                in_class = false;
                current.push(character);
            }
            '|' if !in_class => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(character),
        }
    }
    if escaped {
        return Err(format!("pattern \"{source}\": trailing \"\\\" with nothing to escape"));
    }
    parts.push(current);
    Ok(parts)
}

fn compile_branch(source: &str) -> Result<Branch, String> {
    let mut chars: Vec<char> = source.chars().collect();
    let mut anchored_start = false;
    let mut anchored_end = false;

    if chars.first() == Some(&'^') {
        anchored_start = true;
        chars.remove(0);
    }
    // Un `$` final n'est une ancre que s'il n'est pas échappé ; la parité des
    // antislashs qui le précèdent tranche.
    if chars.last() == Some(&'$') {
        let backslashes = chars.iter().rev().skip(1).take_while(|c| **c == '\\').count();
        if backslashes % 2 == 0 {
            anchored_end = true;
            chars.pop();
        }
    }

    let mut pieces = Vec::new();
    let mut position = 0usize;
    while position < chars.len() {
        let atom = match chars[position] {
            '(' | ')' => {
                return Err(format!("pattern \"{source}\": groups \"( )\" are not supported"));
            }
            '{' => {
                return Err(format!(
                    "pattern \"{source}\": quantifiers \"{{n,m}}\" are not supported"
                ));
            }
            '\\' => {
                position += 1;
                let escaped = *chars
                    .get(position)
                    .ok_or_else(|| format!("pattern \"{source}\": trailing \"\\\""))?;
                Atom::Literal(escaped)
            }
            '.' => Atom::Any,
            '[' => {
                let (class, consumed) = compile_class(&chars[position..], source)?;
                position += consumed - 1;
                class
            }
            '*' | '+' | '?' => {
                return Err(format!(
                    "pattern \"{source}\": quantifier \"{}\" with nothing to repeat",
                    chars[position]
                ));
            }
            other => Atom::Literal(other),
        };
        position += 1;

        let repeat = match chars.get(position) {
            Some('*') => {
                position += 1;
                Repeat::ZeroOrMore
            }
            Some('+') => {
                position += 1;
                Repeat::OneOrMore
            }
            Some('?') => {
                position += 1;
                Repeat::ZeroOrOne
            }
            _ => Repeat::One,
        };
        pieces.push(Piece { atom, repeat });
    }

    Ok(Branch { anchored_start, anchored_end, pieces })
}

/// Analyse `[...]` et renvoie la classe ainsi que le nombre de caractères consommés.
fn compile_class(chars: &[char], source: &str) -> Result<(Atom, usize), String> {
    let mut position = 1; // saute le '['
    let negated = chars.get(position) == Some(&'^');
    if negated {
        position += 1;
    }
    let mut ranges: Vec<(char, char)> = Vec::new();
    // Un ']' en première position est un littéral, comme dans les regex POSIX.
    let mut first = true;

    while position < chars.len() {
        let current = chars[position];
        if current == ']' && !first {
            return Ok((Atom::Class { negated, ranges }, position + 1));
        }
        first = false;
        let low = if current == '\\' {
            position += 1;
            *chars.get(position).ok_or_else(|| format!("pattern \"{source}\": trailing \"\\\""))?
        } else {
            current
        };
        // `a-z` : un tiret suivi d'autre chose que le ']' fermant forme un intervalle.
        if chars.get(position + 1) == Some(&'-')
            && chars.get(position + 2).is_some_and(|next| *next != ']')
        {
            let high = chars[position + 2];
            if high < low {
                return Err(format!("pattern \"{source}\": range \"{low}-{high}\" is reversed"));
            }
            ranges.push((low, high));
            position += 3;
        } else {
            ranges.push((low, low));
            position += 1;
        }
    }
    Err(format!("pattern \"{source}\": unclosed character class"))
}

fn atom_matches(atom: &Atom, character: char) -> bool {
    match atom {
        Atom::Literal(expected) => *expected == character,
        Atom::Any => true,
        Atom::Class { negated, ranges } => {
            let inside = ranges.iter().any(|(low, high)| character >= *low && character <= *high);
            inside != *negated
        }
    }
}

fn branch_matches(branch: &Branch, text: &[char]) -> bool {
    if branch.anchored_start {
        return match_here(branch, 0, text, 0);
    }
    // Non ancré : on tente chaque position de départ, y compris la fin de chaîne
    // pour qu'un motif entièrement optionnel corresponde.
    (0..=text.len()).any(|start| match_here(branch, 0, text, start))
}

/// Retour arrière récursif sur les pièces du motif.
///
/// La profondeur de récursion est bornée par la longueur du texte, elle-même bornée
/// par la troncature des étiquettes SNMP : pas de risque de débordement de pile.
fn match_here(branch: &Branch, piece_index: usize, text: &[char], position: usize) -> bool {
    let Some(piece) = branch.pieces.get(piece_index) else {
        return !branch.anchored_end || position == text.len();
    };

    match piece.repeat {
        Repeat::One => {
            position < text.len()
                && atom_matches(&piece.atom, text[position])
                && match_here(branch, piece_index + 1, text, position + 1)
        }
        Repeat::ZeroOrOne => {
            (position < text.len()
                && atom_matches(&piece.atom, text[position])
                && match_here(branch, piece_index + 1, text, position + 1))
                || match_here(branch, piece_index + 1, text, position)
        }
        Repeat::ZeroOrMore | Repeat::OneOrMore => {
            let minimum = usize::from(piece.repeat == Repeat::OneOrMore);
            let mut consumed = 0usize;
            while position + consumed < text.len()
                && atom_matches(&piece.atom, text[position + consumed])
            {
                consumed += 1;
            }
            // Gourmand puis retour arrière, comme une regex classique.
            while consumed >= minimum {
                if match_here(branch, piece_index + 1, text, position + consumed) {
                    return true;
                }
                if consumed == 0 {
                    break;
                }
                consumed -= 1;
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(source: &str) -> Pattern {
        Pattern::compile(source).unwrap_or_else(|error| panic!("« {source} » : {error}"))
    }

    #[test]
    fn joker_universel() {
        let motif = compile("*");
        assert!(motif.is_wildcard());
        assert!(motif.is_match(""));
        assert!(motif.is_match("n'importe quoi"));
    }

    #[test]
    fn ancres() {
        assert!(compile("^lo$").is_match("lo"));
        assert!(!compile("^lo$").is_match("lo0"));
        assert!(!compile("^lo$").is_match("eth-lo"));
        assert!(compile("^veth").is_match("veth1a2b"));
        assert!(!compile("^veth").is_match("br-veth"));
        assert!(compile("0$").is_match("eth0"));
        assert!(!compile("0$").is_match("eth01"));
    }

    #[test]
    fn recherche_non_ancree() {
        assert!(compile("docker").is_match("br-docker0"));
        assert!(compile("Linux").is_match("Linux nas 6.1.0 x86_64"));
        assert!(!compile("Linux").is_match("Cisco IOS Software"));
    }

    #[test]
    fn point_et_quantificateurs() {
        assert!(compile("Linux.*").is_match("Linux"));
        assert!(compile("Linux.*x86").is_match("Linux nas 6.1.0 x86_64"));
        assert!(!compile("^Linux.*x86$").is_match("Linux nas 6.1.0 x86_64 GNU"));
        assert!(compile("^a+b$").is_match("aaab"));
        assert!(!compile("^a+b$").is_match("b"));
        assert!(compile("^a?b$").is_match("b"));
        assert!(compile("^a?b$").is_match("ab"));
        assert!(!compile("^a?b$").is_match("aab"));
    }

    #[test]
    fn classes_de_caracteres() {
        assert!(compile("^eth[0-9]+$").is_match("eth12"));
        assert!(!compile("^eth[0-9]+$").is_match("eth"));
        assert!(!compile("^eth[0-9]+$").is_match("ethX"));
        assert!(compile("^[^v]").is_match("eth0"));
        assert!(!compile("^[^v]").is_match("veth0"));
        assert!(compile("^[abc]$").is_match("b"));
    }

    #[test]
    fn alternation_de_premier_niveau() {
        let motif = compile("^lo$|^docker[0-9]*$");
        assert!(motif.is_match("lo"));
        assert!(motif.is_match("docker0"));
        assert!(motif.is_match("docker"));
        assert!(!motif.is_match("eth0"));
    }

    #[test]
    fn echappement() {
        assert!(compile(r"^1\.3\.6").is_match("1.3.6"));
        assert!(!compile(r"^1\.3\.6").is_match("1x3x6"));
        assert!(compile(r"\$$").is_match("prix$"));
    }

    #[test]
    fn les_motifs_non_geres_sont_refuses_a_la_compilation() {
        for invalide in ["(ab)+", "a{2,3}", "[a-", "*abc", "ab\\", "[z-a]"] {
            assert!(Pattern::compile(invalide).is_err(), "« {invalide} » aurait dû être refusé");
        }
    }

    #[test]
    fn le_retour_arriere_trouve_la_bonne_decoupe() {
        // Le quantificateur gourmand doit rendre des caractères pour laisser la
        // suite du motif correspondre.
        assert!(compile("^a.*a$").is_match("abcabca"));
        assert!(compile("^.*0$").is_match("eth0"));
        assert!(!compile("^.*0$").is_match("eth1"));
    }
}
