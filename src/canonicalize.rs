//! Converts the MathML to some sort of canonical MathML.
//!
//! Some changes made:
//! * &extra whitespace at the start/end of tokens is trimmed.
//! * "equivalent" characters are converted to a chosen character.
//! * known "bad" MathML is cleaned up (this will likely be an ongoing effort)
//! * mrows are added based on operator priorities from the MathML Operator Dictionary
#![allow(clippy::needless_return)]
use crate::errors::*;
use sxd_document::dom::*;
use sxd_document::QName;
use phf::{phf_map, phf_set};
use crate::xpath_functions::{IsBracketed, is_leaf};
use std::{ptr::eq as ptr_eq};
use crate::pretty_print::*;
use regex::Regex;
use std::fmt;
use crate::chemistry::*;

// FIX: DECIMAL_SEPARATOR should be set by env, or maybe language
const DECIMAL_SEPARATOR: &str = ".";
pub const CHANGED_ATTR: &str = "data-changed";
pub const ADDED_ATTR_VALUE: &str = "added";
const MFENCED_ATTR_VALUE: &str = "from_mfenced";
// character to use instead of the text content for priority, etc.
pub const CHEMICAL_BOND: &str ="data-chemical-bond";

/// Used when mhchem is detected and we should favor postscripts rather than prescripts in constructing an mmultiscripts
const MHCHEM_MMULTISCRIPTS_HACK: &str = "MHCHEM_SCRIPT_HACK";

// (perfect) hash of operators built from MathML's operator dictionary
static OPERATORS: phf::Map<&str, OperatorInfo> = include!("operator-info.in");


// The set of fence operators that can being either a left or right fence (or infix). For example: "|".
static AMBIGUOUS_OPERATORS: phf::Set<&str> = phf_set! {
	"|", "∥", "\u{2016}"
};

// static vars used when canonicalizing
lazy_static!{
	// lowest priority operator so it is never popped off the stack
	static ref LEFT_FENCEPOST: OperatorInfo = OperatorInfo{ op_type: OperatorTypes::LEFT_FENCE, priority: 0, next: &None };

	static ref INVISIBLE_FUNCTION_APPLICATION: &'static OperatorInfo = OPERATORS.get("\u{2061}").unwrap();
	static ref IMPLIED_TIMES: &'static OperatorInfo = OPERATORS.get("\u{2062}").unwrap();
	static ref IMPLIED_INVISIBLE_COMMA: &'static OperatorInfo = OPERATORS.get("\u{2063}").unwrap();
	static ref IMPLIED_INVISIBLE_PLUS: &'static OperatorInfo = OPERATORS.get("\u{2064}").unwrap();

	// FIX: any other operators that should act the same (e.g, plus-minus and minus-plus)?
	static ref PLUS: &'static OperatorInfo = OPERATORS.get("+").unwrap();
	static ref MINUS: &'static OperatorInfo = OPERATORS.get("-").unwrap();
	static ref PREFIX_MINUS: &'static OperatorInfo = MINUS.next.as_ref().unwrap();

	static ref TIMES_SIGN: &'static OperatorInfo = OPERATORS.get("×").unwrap();

	// IMPLIED_TIMES_HIGH_PRIORITY -- used in trig functions for things like sin 2x cos 2x where want > function app priority
	static ref IMPLIED_TIMES_HIGH_PRIORITY: OperatorInfo = OperatorInfo{
		op_type: OperatorTypes::INFIX, priority: 851, next: &None
	};
	// IMPLIED_SEPARATOR_HIGH_PRIORITY -- used for Geometry points like ABC
	static ref IMPLIED_SEPARATOR_HIGH_PRIORITY: OperatorInfo = OperatorInfo{
		op_type: OperatorTypes::INFIX, priority: 901, next: &None
	};
	// IMPLIED_CHEMICAL_BOND -- used for implicit and explicit bonds
	static ref IMPLIED_CHEMICAL_BOND: OperatorInfo = OperatorInfo{
		op_type: OperatorTypes::INFIX, priority: 905, next: &None
	};
	static ref IMPLIED_PLUS_SLASH_HIGH_PRIORITY: OperatorInfo = OperatorInfo{	// (linear) mixed fraction 2 3/4
		op_type: OperatorTypes::INFIX, priority: 881, next: &None
	};

	// Useful static defaults to have available if there is no character match
	static ref DEFAULT_OPERATOR_INFO_PREFIX: &'static OperatorInfo = &OperatorInfo{
		op_type: OperatorTypes::PREFIX, priority: 260, next: &None
	};
	static ref DEFAULT_OPERATOR_INFO_INFIX: &'static OperatorInfo = &OperatorInfo{
		op_type: OperatorTypes::INFIX, priority: 260, next:& None
	};
	static ref DEFAULT_OPERATOR_INFO_POSTFIX: &'static OperatorInfo = &OperatorInfo{
		op_type: OperatorTypes::POSTFIX, priority: 260, next: &None
	};

	// avoids having to use Option<OperatorInfo> in some cases
	static ref ILLEGAL_OPERATOR_INFO: &'static OperatorInfo = &OperatorInfo{
		op_type: OperatorTypes::INFIX, priority: 999, next: &None
	};

	// used to tell if an operator is a relational operator
	static ref EQUAL_PRIORITY: usize = OPERATORS.get("=").unwrap().priority;

	// useful for detecting whitespace
	static ref IS_WHITESPACE: Regex = Regex::new(r"^\s+$").unwrap();    // only Unicode whitespace
}

// Operators are either PREFIX, INFIX, or POSTFIX, but can also have other properties such as LEFT_FENCE
bitflags! {
	struct OperatorTypes: u32 {
		const NONE		= 0x0;
		const PREFIX	= 0x1;
		const INFIX		= 0x2;
		const POSTFIX	= 0x4;
		const FENCE		= 0x8;
		const LEFT_FENCE= 0x9;
		const RIGHT_FENCE=0xc;
		const UNSPECIFIED=0xf;		// 'and-ing will match anything
	}
}

// OperatorInfo is a key structure for parsing.
// They OperatorInfo is this program's representation of MathML's Operator Dictionary.
// The OperatorTypes say how the operator can group (can be overridden with @form="..." on an element).
//   Basically, it says the operator can be at the start, middle, or end of an mrow.
// The priority field gives the relationships between operators so that lower priority operators are towards the root of the tree.
//   E.g.,  '=' is lower priority than (infix) '+', which in turn is lower priority than multiplication.
// The operator info is a linked list because some operators (not many) have alternatives (e.g, '+' is both prefix and infix)
// All OperatorInfo is static info, with some special static defaults to capture when it is not listed in the operator dictionary.
#[derive(Clone, Debug)]
struct OperatorInfo {
	op_type: OperatorTypes,		// can be set on <mo>
	priority: usize,			// not settable on an element
	next: &'static Option<OperatorInfo>,	// can be both prefix & infix (etc) -- chain of options
}

// The character is separated out from the OperatorInfo as this allows the OperatorInfo to be static (can use default values)
#[derive(Clone, Debug)]
struct OperatorPair<'op> {
	ch: &'op str,
	op: &'static OperatorInfo
}

impl<'op> OperatorPair<'op> {
	fn new() -> OperatorPair<'op> {
		return OperatorPair{
			ch: "illegal",					// value 'illegal' used only in debugging, if then
			op: &ILLEGAL_OPERATOR_INFO,		// ILLEGAL_OPERATOR_INFO avoids using <Option>
		};
	}
}

// OperatorVersions is a convenient data structure when looking to see whether the operator should be prefix, infix, or postfix.
// It is only used in one place in the code, so this could maybe be eliminated and the code localized to where it is used.
#[derive(Debug)]
struct OperatorVersions {
	prefix: Option<&'static OperatorInfo>,
	infix: Option<&'static OperatorInfo>,
	postfix: Option<&'static OperatorInfo>,
}

impl OperatorVersions {
	fn new(op: &'static OperatorInfo) -> OperatorVersions {
		let mut op = op;
		let mut prefix = None;
		let mut infix = None;
		let mut postfix = None;
		loop {
			if op.is_prefix() {
				prefix = Some( op );
			} else if op.is_infix() {
				infix = Some( op )
			} else if op.is_postfix() {
				postfix = Some( op );
			} else {
				panic!("OperatorVersions::new: operator is not prefix, infix, or postfix")
			}
			//let another_op = op.next;
			match &op.next {
				None => break,
				Some(alt_op) => op = alt_op,
			}
		}
		return OperatorVersions{prefix, infix, postfix};
	}
}


impl OperatorInfo {
	fn is_prefix(&self) -> bool {
		return (self.op_type.bits & OperatorTypes::PREFIX.bits) != 0;
	}

	fn is_infix(&self) -> bool {
		return (self.op_type.bits & OperatorTypes::INFIX.bits) != 0;
	}

	fn is_postfix(&self) -> bool {
		return (self.op_type.bits & OperatorTypes::POSTFIX.bits) != 0;
	}

	fn is_left_fence(&self) -> bool {
		return self.op_type.bits & OperatorTypes::LEFT_FENCE.bits == OperatorTypes::LEFT_FENCE.bits;
	}

	fn is_right_fence(&self) -> bool {
		return self.op_type.bits & OperatorTypes::RIGHT_FENCE.bits ==OperatorTypes::RIGHT_FENCE.bits;
	}

	fn is_fence(&self) -> bool {
		return (self.op_type.bits & (OperatorTypes::LEFT_FENCE.bits | OperatorTypes::RIGHT_FENCE.bits)) != 0;
	}

	fn is_operator_type(&self, op_type: OperatorTypes) -> bool {
		return self.op_type.bits & op_type.bits != 0;
	}

	fn is_plus_or_minus(&self) -> bool {
		return ptr_eq(self, *PLUS) || ptr_eq(self, *MINUS);
	}

	fn is_times(&self) -> bool {
		return ptr_eq(self, *IMPLIED_TIMES) || ptr_eq(self, *TIMES_SIGN);
	}

	fn is_nary(&self, previous_op: &OperatorInfo) -> bool {
		return	ptr_eq(previous_op,self) ||
				(previous_op.is_plus_or_minus() && self.is_plus_or_minus()) ||
				(previous_op.is_times() && self.is_times());
	}
}

// StackInfo contains all the needed information for deciding shift/reduce during parsing.
// The stack itself is just a Vec of StackInfo (since we only push, pop, and look at the top)
// There are a number of useful functions defined on StackInfo. 
struct StackInfo<'a, 'op>{
	mrow: Element<'a>,			// mrow being built
	op_pair: OperatorPair<'op>,	// last operator placed on stack
	is_operand: bool,			// true if child at end of mrow is an operand (as opposed to an operator)
}

impl<'a, 'op> fmt::Display for StackInfo<'a, 'op> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "StackInfo(op={}/{}, is_operand={}, mrow({}",
				show_invisible_op_char(self.op_pair.ch), self.op_pair.op.priority, self.is_operand,
				if self.mrow.children().is_empty() {")"} else {""})?;
		for child in self.mrow.children() {
			let child = as_element(child);
			write!(f, "{}{}", name(&child), if child.following_siblings().is_empty() {")"} else {","})?;
		}
        return Ok( () );
    }
}

impl<'a, 'op:'a> StackInfo<'a, 'op> {
	fn new(doc: Document<'a>) -> StackInfo<'a, 'op> {
		// debug!("  new empty StackInfo");
		let mrow = create_mathml_element(&doc, "mrow") ;
		mrow.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
		return StackInfo{
			mrow,
			op_pair: OperatorPair{ ch: "\u{E000}", op: &LEFT_FENCEPOST },
			is_operand: false,
		}
	}

	fn with_op<'d>(doc: &'d Document<'a>, node: Element<'a>, op_pair: OperatorPair<'op>) -> StackInfo<'a, 'op> {
		// debug!("  new StackInfo with '{}' and operator {}/{}", name(&node), show_invisible_op_char(op_pair.ch), op_pair.op.priority);
		let mrow = create_mathml_element(doc, "mrow");
		mrow.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
		mrow.append_child(node);
		return StackInfo {
			mrow,
			op_pair,
			is_operand: false,
		}
	}

	fn priority(&self) -> usize {
		return self.op_pair.op.priority;
	}

	fn last_child_in_mrow(&self) -> Option<Element<'a>> {
		let children = self.mrow.children();
		if children.is_empty() {
			return None
		} else {
			return Some( as_element(children[children.len() - 1]) );
		}
	}

	fn add_child_to_mrow(&mut self, child: Element<'a>, child_op: OperatorPair<'op>) {
		// debug!("  adding '{}' to mrow[{}], operator '{}/{}'",
		// 		element_summary(child), self.mrow.children().len(), show_invisible_op_char(child_op.ch), child_op.op.priority);
		self.mrow.append_child(child);
		if ptr_eq(child_op.op, *ILLEGAL_OPERATOR_INFO) {
			assert!(!self.is_operand); 	// should not have two operands in a row
			self.is_operand = true;
		} else {
			self.op_pair = child_op;
			self.is_operand = false;
		}
	}

	fn remove_last_operand_from_mrow(&mut self) -> Element<'a> {
		let children = self.mrow.children();
		assert!( !children.is_empty() );
		assert!( self.is_operand || children.len()==1 );		// could be operator that is forced to be interpreted as operand -- eg, bad input like "x+("
		self.is_operand = false;
		let last_operand = as_element(children[children.len()-1]);
		// debug!("  Removing last element '{}' from mrow[{}]",element_summary(last_operand), children.len());
		last_operand.remove_from_parent();
		return last_operand;
	}

}


pub fn create_mathml_element<'a>(doc: &Document<'a>, name: &str) -> Element<'a> {
	return doc.create_element(sxd_document::QName::with_namespace_uri(
		Some("http://www.w3.org/1998/Math/MathML"),
		name));
}

pub fn is_fence(mo: Element) -> bool {
	return CanonicalizeContext::new()
			.find_operator(mo, None, None, None).is_fence();
}

pub fn is_relational_op(mo: Element) -> bool {
	return CanonicalizeContext::new()
			.find_operator(mo, None, None, None).priority == *EQUAL_PRIORITY;
}

pub fn set_mathml_name(element: Element, new_name: &str) {
	element.set_name(QName::with_namespace_uri(Some("http://www.w3.org/1998/Math/MathML"), new_name));
}

pub fn replace_children<'a>(mathml: Element<'a>, replacements: Vec<Element<'a>>) -> Element<'a> {
	// replace the children of the parent (must exist since this only happens for leaves) with the new children
	if replacements.len() == 1 {
		// rather than replace the children, the children are already in place, so we can optimize a little
		add_attrs(mathml, replacements[0].attributes());
		return mathml;
	}

	let parent = mathml.parent().unwrap().element().unwrap();
	if ELEMENTS_WITH_FIXED_NUMBER_OF_CHILDREN.contains(name(&parent)) {
		// make sure that MAYBE_CHEMISTRY is set since we aren't using the new element directly
		add_attrs(mathml, replacements[0].attributes());

		// wrap in an mrow
		let mrow = create_mathml_element(&mathml.document(), "mrow");
		mrow.append_children(replacements);
		return mathml;
	} else {
		// replace the children of the parent with 'replacements' inserted in place of 'mathml'
		let mut new_children = mathml.preceding_siblings();
		let i_first_new_child = new_children.len();
		let mut replacements = replacements.iter().map(|&el| ChildOfElement::Element(el)).collect::<Vec<ChildOfElement>>();
		new_children.append(&mut replacements);
		new_children.append(&mut mathml.following_siblings());
		parent.replace_children(new_children);
		return as_element(parent.children()[i_first_new_child]);
	}
}

// returns the presentation element of a "semantics" element
pub fn get_presentation_element(element: Element) -> (usize, Element) {
	// FIX: implement this
	assert_eq!(name(&element), "semantics");
	let children = element.children();
	if let Some( (i, child) ) = children.iter().enumerate().find(|(_, &child)| 
			if let Some(encoding) = as_element(child).attribute_value("encoding") {
				encoding == "MathML-Presentation"
			} else {
				false
			})
	{
		let presentation_annotation = as_element(*child);
		// debug!("get_presentation_element:\n{}", mml_to_string(&presentation_annotation));
		assert_eq!(presentation_annotation.children().len(), 1);
		return (i, as_element(presentation_annotation.children()[0]));
	} else {
		return (0, as_element(children[0]));
	}
}

/// Canonicalize does several things:
/// 1. cleans up the tree so all extra white space is removed (should only have element and text nodes)
/// 2. normalize the characters
/// 3. clean up "bad" MathML based on known output from some converters (TODO: still a work in progress)
/// 4. the tree is "parsed" based on the mo (priority)/mi/mn's in an mrow
///    *  this adds mrows mrows and some invisible operators (implied times, function app, ...)
///    * extra mrows are removed
///    * implicit mrows are turned into explicit mrows (e.g, there will be a single child of 'math')
///
/// Canonicalize is pretty conservative in adding new mrows and won't do it if:
/// * there is an intent attr
/// * if the mrow starts and ends with a fence (e.g, French open interval "]0,1[")
///
/// An mrow is never deleted unless it is redundant.
pub fn canonicalize(mathml: Element) -> Result<Element> {
	let context = CanonicalizeContext::new();
	return context.canonicalize(mathml);
}

struct CanonicalizeContext {
}

#[derive(PartialEq)]
#[allow(non_camel_case_types)] 
enum DigitBlockType {
	None,
	DecimalBlock_3,
	DecimalBlock_4,
	DecimalBlock_5,
	BinaryBlock_4,
}


#[derive(Debug, PartialEq)]
enum FunctionNameCertainty {
	True,
	Maybe,
	False
}


static ELEMENTS_WITH_ONE_CHILD: phf::Set<&str> = phf_set! {
	"math", "msqrt", "merror", "mpadded", "mphantom", "menclose", "mtd", "mscarry"
};

static ELEMENTS_WITH_FIXED_NUMBER_OF_CHILDREN: phf::Set<&str> = phf_set! {
	"mfrac", "mroot", "msub", "msup", "msubsup","munder", "mover", "munderover", "mmultiscripts", "mlongdiv"
};

static EMPTY_ELEMENTS: phf::Set<&str> = phf_set! {
	"mspace", "none", "mprescripts", "mglyph", "malignmark", "maligngroup", "msline",
};

lazy_static! {
	// turns out Roman Numerals tests aren't needed, but we do want to block VII from being a chemical match
	// two cases because we don't want to have a match for 'Cl', etc.
	static ref UPPER_ROMAN_NUMERAL: Regex = Regex::new(r"^\s*^M{0,3}(CM|CD|D?C{0,3})(XC|XL|L?X{0,3})(IX|IV|V?I{0,3})\s*$").unwrap();
	static ref LOWER_ROMAN_NUMERAL: Regex = Regex::new(r"^\s*^m{0,3}(cm|cd|d?c{0,3})(xc|xl|l?x{0,3})(ix|iv|v?i{0,3})\s*$").unwrap();
}


impl CanonicalizeContext {
	fn new() -> CanonicalizeContext {
		return CanonicalizeContext{}
	}

	fn canonicalize<'a>(&self, mut mathml: Element<'a>) -> Result<Element<'a>> {
		// debug!("MathML before canonicalize:\n{}", mml_to_string(&mathml));
	
		if name(&mathml) != "math" {
			// debug!("Didn't start with <math> element -- attempting repair");
			let math_element = create_mathml_element(&mathml.document(), "math");
			math_element.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
			math_element.append_child(mathml);
			let root = math_element.document().root();
			root.clear_children();
			root.append_child(math_element);
			mathml = root.children()[0].element().unwrap();
		}
		CanonicalizeContext::assure_mathml(mathml)?;
		let mathml = self.clean_mathml(mathml).unwrap();	// 'math' is never removed
		self.assure_math_not_empty(mathml);
		self.assure_nary_tag_has_mrow(mathml);
		let mut converted_mathml = self.canonicalize_mrows(mathml)
				.chain_err(|| format!("while processing\n{}", mml_to_string(&mathml)))?;
		if !crate::chemistry::scan_and_mark_chemistry(converted_mathml) {
			debug!("Not chemistry -- retry:\n{}", mml_to_string(&converted_mathml));
			self.assure_nary_tag_has_mrow(converted_mathml);
			converted_mathml = self.canonicalize_mrows(mathml)
				.chain_err(|| format!("while processing\n{}", mml_to_string(&mathml)))?;
		}
		debug!("\nMathML after canonicalize:\n{}", mml_to_string(&converted_mathml));
		return Ok(converted_mathml);
	}
	
	/// Make sure there is some content inside the <math> tag
	fn assure_math_not_empty(&self, mathml: Element) {
		assert_eq!(name(&mathml), "math");
		if mathml.children().is_empty() {
			let child = CanonicalizeContext::create_empty_element(&mathml.document());
			mathml.append_child(child);
		}
	}
	
	fn assure_nary_tag_has_mrow(&self, mathml: Element) {
		let children = mathml.children();
		if children.len() > 1 && ELEMENTS_WITH_ONE_CHILD.contains(name(&mathml)) {
			// wrap the children in an mrow
			let mrow = create_mathml_element(&mathml.document(), "mrow");
			mrow.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
			mrow.append_children(children);
			mathml.replace_children(vec![ChildOfElement::Element(mrow)]);
		}
	}

	/// Return an error is some element is not MathML (only look at first child of <semantics>) or if it has the wrong number of children
	fn assure_mathml(mathml: Element) -> Result<()> {
		static ALL_MATHML_ELEMENTS: phf::Set<&str> = phf_set!{
			"mi", "mo", "mn", "mtext", "ms", "mspace", "mglyph",
			"mfrac", "mroot", "msub", "msup", "msubsup","munder", "mover", "munderover", "mmultiscripts",
			"mstack", "mlongdiv", "msgroup", "msrow", "mscarries", "mscarry", "msline",
			"none", "mprescripts", "malignmark", "maligngroup",
			"math", "msqrt", "merror", "mpadded", "mphantom", "menclose", "mtd", "mstyle",
			"mrow", "mfenced", "mtable", "mtr", "mlabeledtr",
		};

		let n_children = mathml.children().len();
		let element_name = name(&mathml);
		if is_leaf(mathml) {
			if EMPTY_ELEMENTS.contains(element_name) {
				if n_children != 0 {
					bail!("{} should only have one child:\n{}", element_name, mml_to_string(&mathml));
				}
			} else if (n_children == 1 && mathml.children()[0].text().is_some()) || n_children == 0 {  // allow empty children such as mtext
				return Ok( () );
			} else {
				bail!("Not a valid MathML leaf element:\n{}", mml_to_string(&mathml));
			};
		}

		if ELEMENTS_WITH_FIXED_NUMBER_OF_CHILDREN.contains(element_name) {
			match element_name {
				"munderover" | "msubsup" => if n_children != 3 {
					bail!("{} should have 3 children:\n{}", element_name, mml_to_string(&mathml));
				},
				"mmultiscripts" => {
					let has_prescripts = mathml.children().iter()
							.any(|&child| name(&as_element(child)) == "mprescripts");
					if has_prescripts ^ (n_children % 2 == 0) {
						bail!("{} has the wrong number of children:\n{}", element_name, mml_to_string(&mathml));
					}
				},
				"mlongdiv" => if n_children < 3 {
					bail!("{} should have at least 3 children:\n{}", element_name, mml_to_string(&mathml));
				},
				_ => if n_children != 2 {
					bail!("{} should have 2 children:\n{}", element_name, mml_to_string(&mathml));
				},
			}
		}
		let children = mathml.children();
		if element_name == "semantics" {
			if children.is_empty() {
				return Ok( () );
			} else {
				return CanonicalizeContext::assure_mathml(get_presentation_element(mathml).1);
			}
		}
		if !ALL_MATHML_ELEMENTS.contains(element_name) {
			bail!("'{}' is not a valid MathML element", element_name);
		}
		// valid MathML element and not a leaf -- check the children
		for child in children {
			CanonicalizeContext::assure_mathml( as_element(child) )?;
		}
		return Ok( () );
	}

	fn make_empty_element(mathml: Element) -> Element {
		set_mathml_name(mathml, "mtext");
		mathml.clear_children();
		mathml.set_text("\u{A0}");
		mathml.set_attribute_value("data-changed", "empty_content");
		return mathml;
	}
	
	fn create_empty_element<'a>(doc: &Document<'a>) -> Element<'a> {
		let mtext = create_mathml_element(doc, "mtext");
		mtext.set_text("\u{A0}");
		mtext.set_attribute_value("data-added", "missing-content");
		return mtext;
	}
	
	fn is_empty_element(el: Element) -> bool {
		return (is_leaf(el) && as_text(el).trim().is_empty()) ||
			   (name(&el) == "mrow" && el.children().is_empty());
	}

	/// This function does some cleanup of MathML (mostly fixing bad MathML)
	/// Unlike the main canonicalization routine, significant tree changes happen here
	/// Changes to "good" MathML:
	/// 1. mfenced -> mrow
	/// 2. mspace and mtext with only whitespace are canonicalized to a non-breaking space and merged in with 
	///    an adjacent non-mo element unless in a required element position (need to keep for braille)
	/// 
	/// Note: mspace that is potentially part of a number that was split apart is merged into a number as a single space char
	/// 
	/// mstyle, mpadded, and mphantom, malignmark, maligngroup are removed (but children might be kept)
	/// 
	/// Significant changes are made cleaning up empty bases of scripts, looking for chemistry, merging numbers with commas,
	///   "arg trig" functions, pseudo scripts, and others
	/// 
	/// Returns 'None' if the element should not be in the tree.
	fn clean_mathml<'a>(&self, mathml: Element<'a>) -> Option<Element<'a>> {
		// Note: this works bottom-up (clean the children first, then this element)
		lazy_static! {
			static ref IS_PRIME: Regex = Regex::new(r"['′″‴⁗]").unwrap(); 
        }

		static CURRENCY_SYMBOLS: phf::Set<&str> = phf_set! {
			"$", "¢", "€", "£", "₡", "₤", "₨", "₩", "₪", "₱", "₹", "₺", "₿" // could add more currencies...
		};
		
		// begin by cleaning up empty elements
		// debug!("clean_mathml\n{}", mml_to_string(&mathml));
		let element_name = name(&mathml);
		let parent_name = if element_name == "math" {
			"math".to_string()
		} else {
			let parent = mathml.parent().unwrap().element().unwrap();
			name(&parent).to_string()
		};
		let parent_requires_child = ELEMENTS_WITH_FIXED_NUMBER_OF_CHILDREN.contains(&parent_name);

		// handle empty leaves -- leaving it empty causes problems with the speech rules
		if is_leaf(mathml) && !EMPTY_ELEMENTS.contains(element_name) && as_text(mathml).is_empty() {
			if !parent_requires_child {
				return None;
			}
			CanonicalizeContext::make_empty_element(mathml);
		};
		
		if mathml.children().is_empty() && !EMPTY_ELEMENTS.contains(element_name) {
			if element_name == "mrow" {
				// if it is an empty mrow that doesn't need to be there, get rid of it. Otherwise, replace it with an mtext
				if parent_requires_child {
					if parent_name == "mmultiscripts" {	// MathML Core dropped "none" in favor of <mrow/>, but MathCAT is written with <none/>
						set_mathml_name(mathml, "none");
						return Some(mathml);
					} else {
						return Some( CanonicalizeContext::make_empty_element(mathml) );
					}
				} else {
					return None;
				}
			} else {
				// create some content so that speech rules don't require special cases
				let mtext = CanonicalizeContext::create_empty_element(&mathml.document());
				mathml.append_child(mtext);
				// return Some(mathml);
			}
		};

		match element_name {
			"mn" => {
				let text = as_text(mathml);
				let mut chars = text.chars();
				let first_char = chars.next().unwrap();		// we have already made sure it is non-empty
				// if !text.trim().is_empty() && is_roman_number_match(text) {
				// 	// people tend to set them in a non-italic font and software makes that 'mtext'
				// 	mathml.set_attribute_value("data-roman-numeral", "true");	// mark for easy detection
				// }
				if first_char == '-' || first_char == '\u{2212}' {
					let doc = mathml.document();
					let mo = create_mathml_element(&doc, "mo");
					let mn = create_mathml_element(&doc, "mn");
					mo.set_text("-");
					mn.set_text(&text[first_char.len_utf8()..]);
					set_mathml_name(mathml, "mrow");
					mathml.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
					mathml.replace_children([mo,mn]);
				}
				return Some(mathml);
			},
			"ms" | "mglyph" => {
				return Some(mathml);
			},
			"mi" => {
				let text = as_text(mathml);
				// if !text.trim().is_empty() && is_roman_number_match(text) && is_roman_numeral_number_context(mathml) {
				// 	// people tend to set them in a non-italic font and software makes that 'mtext'
				// 	set_mathml_name(mathml, "mn");
				// 	mathml.set_attribute_value("data-roman-numeral", "true");	// mark for easy detection
				// 	return Some(mathml);
			 	// }
				if let Some(dash) = canonicalize_dash(text) {		// needs to be before OPERATORS.get due to "--"
					mathml.set_text(dash);
					return Some(mathml);
				} else if OPERATORS.get(text).is_some() {
					set_mathml_name(mathml, "mo");
					return Some(mathml);
				} else if let Some(result) = merge_arc_trig(mathml) {
						return Some(result);
				} else if IS_PRIME.is_match(text) {
					let new_text = merge_prime_text(text);
					mathml.set_text(&new_text);
					return Some(mathml);
				} else if let Some(result) = split_points(mathml) {
					return Some(result);
				} else {
					return Some(mathml);
				};
			},
			"mtext" => {
				if let Some(result) = merge_arc_trig(mathml) {
					return Some(result);
				};
			
				if let Some(result) = split_points(mathml) {
					return Some(result);
				}
				
				let text = as_text(mathml);
				// if !text.trim().is_empty() && is_roman_number_match(text) && is_roman_numeral_number_context(mathml) {
				// 	// people tend to set them in a non-italic font and software makes that 'mtext'
				// 	set_mathml_name(mathml, "mn");
				// 	mathml.set_attribute_value("data-roman-numeral", "true");	// mark for easy detection
				// 	return Some(mathml);
				// }
				// allow non-breaking whitespace to stay -- needed by braille
				let mathml = mathml;
				if IS_WHITESPACE.is_match(text) {
					// normalize to just a single non-breaking space
					CanonicalizeContext::make_empty_element(mathml);
				} else if let Some(dash) = canonicalize_dash(text) {
					mathml.set_text(dash);
				} else if OPERATORS.get(text).is_some() {
					set_mathml_name(mathml, "mo");
					return Some(mathml);
				}
				return if parent_requires_child || !text.is_empty() {Some(mathml)} else {None};
			},
			"mo" => {
				// WIRIS editor puts non-breaking whitespace as standalone in 'mo'
				let text = as_text(mathml);
				if !text.is_empty() && IS_WHITESPACE.is_match(text) {
					// can't throw it out because it is needed by braille -- change to what it really is
					set_mathml_name(mathml, "mtext");
				}
				// common bug: trig functions, lim, etc., should be mi
				// same for ellipsis ("…")
				if let Some(result) = merge_arc_trig(mathml) {
					return Some(result);
				};

				return crate::definitions::DEFINITIONS.with(|definitions| {
					if text == "…" || 
					   definitions.borrow().get_hashset("FunctionNames").unwrap().contains(text) ||
					   definitions.borrow().get_hashset("GeometryShapes").unwrap().contains(text) {
						set_mathml_name(mathml, "mi");
						return Some(mathml);
					}
					if IS_PRIME.is_match(text) {
						let new_text = merge_prime_text(text);
						mathml.set_text(&new_text);
						return Some(mathml);
					}
					if CURRENCY_SYMBOLS.contains(text) {
						set_mathml_name(mathml, "mi");
						return Some(mathml);
					}
					return Some(mathml);
				});
				// note: chemistry test is done later as part of another phase of chemistry cleanup
			},
			"mfenced" => {return self.clean_mathml( convert_mfenced_to_mrow(mathml) )},
			"mstyle" | "mpadded" => {
				// Throw out mstyle and mpadded -- to do this, we need to avoid mstyle being the arg of clean_mathml
				// FIX: should probably push the attrs down to the children (set in 'self')
				let children = mathml.children();
				if children.is_empty() {
					if parent_requires_child {
						// need a placeholder -- make it empty mtext
						return Some( CanonicalizeContext::make_empty_element(mathml));
					} else {
						return None;
					}
				} else if children.len() == 1 {
					// "lift" the child up so all the links (e.g., siblings) are correct
					if let Some(new_mathml) = self.clean_mathml( as_element(children[0]) ) {
						// "lift" the child up so all the links (e.g., siblings) are correct
						mathml.replace_children(new_mathml.children());
						set_mathml_name(mathml, name(&new_mathml));
						add_attrs(mathml, new_mathml.attributes());
						return Some(mathml);
					} else if parent_requires_child {
						// need a placeholder -- make it empty mtext
						return Some( CanonicalizeContext::make_empty_element(mathml));
					} else {
						return None;
					}
				} else {
					// wrap the children in an mrow, but maintain tree siblings by changing mpadded/mstyle to mrow
					set_mathml_name(mathml, "mrow");
					mathml.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
					return self.clean_mathml(mathml);	// now it's an mrow so a different path next time
				}
			},
			"mphantom" | "malignmark" | "maligngroup"=> {
				if parent_requires_child {
					return Some( CanonicalizeContext::make_empty_element(mathml));
				} else {
					return None;
				}
			},
			"mspace" => {
				// need to hold onto space for braille
				let width = mathml.attribute_value("width").unwrap_or("0");
				if is_width_ignorable(width)  {		// testing <= 0 -- could do better
					return None;
				}
				return Some( CanonicalizeContext::make_empty_element(mathml));
			},
			"semantics" => {
				// clean the presentation child but leave the annotations in case they want to be used by the rules.
				// no attempt is made to clean the annotations or verify they are annotations
				// the cleaned child is made the first child and it's annotation-xml wrapper, if any, is removed
				let mut children = mathml.children();
				let (i, presentation) = get_presentation_element(mathml);
				let new_presentation = if let Some(presentation) = self.clean_mathml(presentation) {
					presentation
				} else {
					// probably shouldn't happen, but just in case
					CanonicalizeContext::create_empty_element(&mathml.document())
				};
				if i==0 {
					// common case, so optimize
					children[0] = ChildOfElement::Element(new_presentation);
				} else {
					// rearrange -- inefficient but likely just a few annotation and doesn't happen often
					children.remove(i);
					children.insert(0, ChildOfElement::Element(presentation));
				}
				mathml.replace_children(children);
				return Some(mathml);
			},
			_  => {
				let children = mathml.children();
				if element_name == "mrow" {
					// handle special cases of empty mrows and mrows which just one element
					if children.is_empty() {
						return if parent_requires_child {Some(mathml)} else {None};
					} else if children.len() == 1 {
						let is_from_mhchem = is_from_mhchem_hack(mathml);
						if let Some(new_mathml) = self.clean_mathml(as_element(children[0])) {
							// "lift" the child up so all the links (e.g., siblings) are correct
							mathml.replace_children(new_mathml.children());
							set_mathml_name(mathml, name(&new_mathml));
							add_attrs(mathml, new_mathml.attributes());
							return Some(mathml);
						} else if parent_requires_child {
							let empty = CanonicalizeContext::make_empty_element(mathml);
							if is_from_mhchem {
								empty.set_attribute_value(MHCHEM_MMULTISCRIPTS_HACK, "true");
							}
							return Some(empty);
						} else {
							return None;
						}
					}
				}

				// FIX: this should be setting children, not mathml
				let mathml =  if element_name == "mrow" || ELEMENTS_WITH_ONE_CHILD.contains(element_name) {
					let merged = merge_dots(mathml);	// FIX -- switch to passing in children
					let merged = merge_primes(merged);
					handle_pseudo_scripts(merged)
				} else {
					mathml
				};

				// cleaning children can add or delete subsequent children, so we need to constantly update the children (and mathml)
				let mut children = mathml.children();
				let mut i = 0;
				while i < children.len() {
					if let Some(child) = children[i].element() {
						match self.clean_mathml(child) {
							None => {
								mathml.remove_child(child);
								// don't increment 'i' because there is one less child now and so everything shifted left
							},
							Some(new_child) => {
								let new_child_name = name(&new_child);
								children = mathml.children();				// clean_mathml(child) may have changed following siblings
								// debug!("new_child (i={})\n{}", i, mml_to_string(&new_child));
								children[i] = ChildOfElement::Element(new_child);
								mathml.replace_children(children);
								if new_child_name == "mi" || new_child_name == "mtext" {
									// can't do this above in 'match' because this changes the tree and
									// lifting single element mrows messes with structure in a conflicting way
									clean_chemistry_leaf(as_element(mathml.children()[i]));
								}			
								i += 1;
							}
						}
						children = mathml.children();						// 'children' moved above, so need need new values
					} else {
						// bad mathml such as '<annotation-xml> </annotation-xml>' -- don't add to new_children
						i += 1;
					}
				}

				// could have deleted children so only one child remains -- need to lift it
				if element_name == "mrow" && children.len() == 1 {
					// "lift" the child up so all the links (e.g., siblings) are correct
					let child = as_element(children[0]);
					mathml.replace_children(child.children());
					set_mathml_name(mathml, name(&child));
					add_attrs(mathml, child.attributes());
					return Some(mathml);		// child has already been cleaned, so we can return
				}

				if element_name == "mrow" || ELEMENTS_WITH_ONE_CHILD.contains(element_name) {
					merge_number_blocks(mathml, &mut children);
					merge_whitespace(&mut children);
					handle_convert_to_mmultiscripts(&mut children);

				} else if element_name == "msub" || element_name == "msup" || 
						  element_name == "msubsup" || element_name == "mmultiscripts"{
					if element_name != "mmultiscripts" {
						// mhchem emits some cases that boil down to a completely empty script -- see test mhchem_beta_decay
						let mut is_empty_script = CanonicalizeContext::is_empty_element(as_element(children[0])) &&
						   								CanonicalizeContext::is_empty_element(as_element(children[1]));
						if element_name == "msubsup" {
							is_empty_script = CanonicalizeContext::is_empty_element(as_element(children[2]));
						}
						if is_empty_script {
							if parent_requires_child {
								// need a placeholder -- make it empty mtext
								return Some( as_element(children[0]) );	// pick one of the empty elements
							} else {
								return None;
							}
						}
					}
					let mathml = if element_name == "mmultiscripts" {clean_mmultiscripts(mathml).unwrap()} else {mathml};
					// debug!("some scripted element...\n{}", mml_to_string(&mathml));	
					if !is_chemistry_off() {
						let likely_chemistry = likely_adorned_chem_formula(mathml);
						// debug!("likely_chemistry={}, {}", likely_chemistry, mml_to_string(&mathml));
						if likely_chemistry >= 0 {
							mathml.set_attribute_value(MAYBE_CHEMISTRY, likely_chemistry.to_string().as_str());
						}
					}

					if element_name == "msubsup" {
						return Some( clean_msubsup(mathml) );
					} else {
						return Some(mathml);
					}
				}

				mathml.replace_children(children);
				// debug!("clean_mathml: after loop\n{}", mml_to_string(&mathml));

				if element_name == "mrow" || ELEMENTS_WITH_ONE_CHILD.contains(element_name) {
					clean_chemistry_mrow(mathml);
				}
				self.assure_nary_tag_has_mrow(mathml);
				return Some(mathml);				
			}
		}

		/// Returns substitute text if hyphen sequence should be a short or long dash
		fn canonicalize_dash(text: &str)  -> Option<&str> {
			if text == "--"  {
				return Some("—");	// U+2014 (em dash)
			} else if text == "---" || text == "----" {		// use a regexp to catch a longer sequence?
				return Some("―");	// U+2015 (Horizontal bar)
			} else {
				return None;
			}
		}


		/// Returns true if it detects that this is likely coming from mhchem (msub/msup with mrow/mrow/mpadded width=0/mphantom/mi=A)
		/// This should be called with 'mrow' being the outer mrow
		fn is_from_mhchem_hack(mrow: Element) -> bool {
			assert_eq!(name(&mrow), "mrow");
			assert_eq!(mrow.children().len(), 1);
			let parent = mrow.parent().unwrap().element().unwrap();
			let parent_name = name(&parent);
			if !(parent_name == "msub" || parent_name == "msup") {
				return false;
			}

			let mrow = as_element(mrow.children()[0]);
			if !(name(&mrow) == "mrow" && mrow.children().len() == 1) {
				return false;
			}
			let child = as_element(mrow.children()[0]);
			if !(name(&child) == "mpadded" && child.attribute("width").is_some()) {
				return false;
			}
			if child.attribute_value("width").unwrap() != "0" {
				return false;
			}

			let child = as_element(child.children()[0]);
			if !(name(&child) == "mphantom" && child.children().len() == 1) {
				return false;
			}

			let child = as_element(child.children()[0]);
			return name(&child) == "mi" && as_text(child) == "A";
		}

		/// Returns true if it appears the width is just a spacing tweak rather than really a space.
		/// 
		/// This is not great in that someone could have multiple 'mspace's and together they exceed the threshold, but not individually
		fn is_width_ignorable(width: &str) -> bool {
			// Check to see if above some threshold (0.25em/0.5ex?)
			// FIX: this is far from complete
			if  width == "0" || width.starts_with('-') {	// simple cases
				return true;	
			}
			if let Some(i) = width.find(|ch: char| ch.is_ascii_alphabetic()) {
				let (amount, unit) = width.split_at(i);
				match unit {
					"em" | "rem" => return amount.parse::<f64>().unwrap_or(100.) < 0.25,
					"ex" => return amount.parse::<f64>().unwrap_or(100.) < 0.5,
					"px" => return amount.parse::<f64>().unwrap_or(100.) < 6.1,	// assume 12pt font -- hack
					_ => return false,
				}
			}
			return false;
		}

		fn clean_chemistry_leaf(mathml: Element) -> Element {
			if !(is_chemistry_off() || mathml.attribute(MAYBE_CHEMISTRY).is_some()) {
				assert!(name(&mathml)=="mi" || name(&mathml)=="mtext");
				// this is hack -- VII is more likely to be roman numeral than the molecule V I I so prevent that from happening
				// FIX: come up with a less hacky way to prevent chem element misinterpretation
				let text = as_text(mathml);
				if text.len() > 2 && is_roman_number_match(text) {
					return mathml;
				}
				if let Some(elements) = convert_leaves_to_chem_elements(mathml) {
					// children are already marked as chemical elements
					return replace_children(mathml, elements);
				} else {
					let likely_chemistry = likely_chem_element(mathml);
					if likely_chemistry >= 0 {
						mathml.set_attribute_value(MAYBE_CHEMISTRY, likely_chemistry.to_string().as_str());
					}
				};
			}
			return mathml;
		}

		/// makes sure the structure is correct and also eliminates <none/> pairs
		/// MathML core changed <none/> to <mrow/>. For now (since MathCAT has lots of "none" tests), <mrow/> => <mtext> => <none/>
		/// (used https://chem.libretexts.org/Courses/Saint_Francis_University/CHEM_113%3A_Human_Chemistry_I_(Muino)/13%3A_Nuclear_Chemistry12/13.04%3A_Nuclear_Decay)
		///
		/// This does some dubious repairs when the structure is bad, but not sure what else to do
		fn clean_mmultiscripts(mathml: Element) -> Option<Element> {
			let mut mathml = mathml;
			let children = mathml.children();
			let n = children.len();
			let i_mprescripts =
				if let Some((i,_)) = children.iter().enumerate()
					.find(|(_,&el)| name(&as_element(el)) == "mprescripts") { i } else { n };
			let has_misplaced_mprescripts = i_mprescripts & 1 == 0;  // should be first, third, ... child
			let mut has_proper_number_of_children = if i_mprescripts == n { n & 1 == 0} else { n & 1 != 0 }; // should be odd else even #
			if has_misplaced_mprescripts || !has_proper_number_of_children || has_none_none_script_pair(&children) {
				// need to reset the children
				let mut new_children = Vec::with_capacity(n+2); // adjusting position of mprescripts might add two children
				new_children.push(children[0]);
				// drop none, none script pairs
				let mut i = 1;
				while i < n {
					let child = as_element(children[i]);
					let child_name = name(&child);
					if child_name == "mprescripts" {
						if has_misplaced_mprescripts {
							let mtext = CanonicalizeContext::create_empty_element(&mathml.document());
							new_children.push(ChildOfElement::Element(mtext));
							has_proper_number_of_children = !has_proper_number_of_children;
						}
						new_children.push(children[i]);
						i += 1;
					} else if i+1 < n && child_name == "none" && name(&as_element(children[i+1])) == "none" {
						i += 2;		// found none, none pair
					} else {
						// copy pair
						new_children.push(children[i]);
						new_children.push(children[i+1]);
						i += 2;
					}
				}
				if new_children.len() == 1 {
					mathml = as_element(new_children[0]);
				} else {
					mathml.replace_children(new_children);
				}
			}

			return Some(mathml);

			fn has_none_none_script_pair(children: &[ChildOfElement]) -> bool {
				let mut i = 1;
				let n = children.len();
				while i < n {
					let child = as_element(children[i]);
					let child_name = name(&child);
					if child_name == "mprescripts" {
						i += 1;
					} else if i+1 < n && child_name == "none" && name(&as_element(children[i+1])) == "none" {
						return true;		// found none, none pair
					} else {
						i += 2;
					}
				}
				return false;
			}
		}

		/// converts element if there is an empty subscript or superscript
		fn clean_msubsup(mathml: Element) -> Element {
			let children = mathml.children();
			let subscript = as_element(children[1]);
			let has_subscript = !(name(&subscript) == "mtext" && as_text(subscript).trim().is_empty());
			let superscript = as_element(children[2]);
			let has_superscript = !(name(&superscript) == "mtext" && as_text(superscript).trim().is_empty());
			if has_subscript && has_superscript {
				return mathml;
			} else if has_subscript {
				set_mathml_name(mathml, "msub");
				let children = vec!(children[0], children[1]);
				mathml.replace_children(children);
				return mathml;
			} else if has_superscript {
				set_mathml_name(mathml, "msup");
				let children = vec!(children[0], children[2]);
				mathml.replace_children(children);
				return mathml;
			} else {
				return as_element(children[0]);	// no scripts
			}
		}

		/// If arg is "arc" (with optional space), merge the following element in if a trig function (sibling is deleted)
		fn merge_arc_trig(leaf: Element) -> Option<Element> {
			assert!(is_leaf(leaf));
			let leaf_text = as_text(leaf);
			if !(leaf_text == "arc" || leaf_text == "arc " || leaf_text == "arc " /* non-breaking space */ ) {
				return None;
			}

			let following_siblings = leaf.following_siblings();
			if following_siblings.is_empty() {
				return None;
			}

			let following_sibling = as_element(following_siblings[0]);
			let following_sibling_name = name(&following_sibling);
			if !(following_sibling_name == "mi" || following_sibling_name == "mo" || following_sibling_name == "mtext") {
				return None;
			}

			return crate::definitions::DEFINITIONS.with(|definitions| {
				// change "arc" "cos" to "arccos" -- we look forward because calling loop stores previous node
				let following_text = as_text(following_sibling);
				if definitions.borrow().get_hashset("TrigFunctionNames").unwrap().contains(following_text) {
					let new_text = "arc".to_string() + following_text;
					set_mathml_name(leaf, "mi");
					leaf.set_text(&new_text);
					following_sibling.remove_from_parent();
					return Some(leaf);
				}
				return None;
			})
		}

		fn convert_mfenced_to_mrow(mfenced: Element) -> Element {
			let open = mfenced.attribute_value("open").unwrap_or("(");
			let close = mfenced.attribute_value("close").unwrap_or(")");
			let mut separators= mfenced.attribute_value("separators").unwrap_or(",").chars();
			set_mathml_name(mfenced, "mrow");
			mfenced.remove_attribute("open");
			mfenced.remove_attribute("close");
			mfenced.remove_attribute("separators");
			let children = mfenced.children();
			let mut new_children = Vec::with_capacity(2*children.len() + 1);
			if !open.is_empty() {
				new_children.push(ChildOfElement::Element( create_mo(mfenced.document(), open, MFENCED_ATTR_VALUE)) );
			}
			if !children.is_empty() {
				new_children.push(children[0]);
				for child in &children[1..] {
					let sep = separators.next().unwrap_or(',').to_string();
					new_children.push( ChildOfElement::Element( create_mo(mfenced.document(), &sep, MFENCED_ATTR_VALUE)) );
					new_children.push(*child);
				}
			}
			if !close.is_empty() {
				new_children.push(ChildOfElement::Element( create_mo(mfenced.document(), close, MFENCED_ATTR_VALUE)) );
			}
			mfenced.replace_children(new_children);
			return mfenced;
		}

		fn is_roman_number_match(text: &str) -> bool {
			return UPPER_ROMAN_NUMERAL.is_match(text) || LOWER_ROMAN_NUMERAL.is_match(text);
		}

		/// Return true if 'element' (which is syntactically a roman numeral) is only inside mrows and
		///  if its length is < 3 chars, then there is another roman numeral near it (separated by an operator).
		/// We want to rule out something like 'm' or 'cm' being a roman numeral.
		// fn is_roman_numeral_number_context(mathml: Element) -> bool {
		// 	assert!(name(&mathml)=="mtext" || name(&mathml)=="mi");
		// 	let mut parent = mathml;
		// 	loop {
		// 		parent = parent.parent().unwrap().element().unwrap();
		// 		let current_name = name(&parent);
		// 		if current_name == "math" {
		// 			break;
		// 		} else if current_name != "mrow" {
		// 			return false;
		// 		}
		// 	}
		// 	if as_text(mathml).len() > 2 {
		// 		return true;
		// 	} else {
		// 		let is_upper_case = as_text(mathml).as_bytes()[0].is_ascii_uppercase();	// safe since we know it is a 
		// 		let preceding = mathml.preceding_siblings();
		// 		if !preceding.is_empty() {
		// 			if !is_roman_numeral_adjacent(preceding.iter().rev(), is_upper_case) {
		// 				return false;
		// 			}
		// 		}
		// 		let following = mathml.following_siblings();
		// 		if following.is_empty() {
		// 			return false;		// no context and too short to confirm it is a roman numeral
		// 		}
		// 		return is_roman_numeral_adjacent(following.iter(), is_upper_case);
		// 	}

		// 	/// make sure all the non-mo leaf siblings are roman numerals
		// 	fn is_roman_numeral_adjacent<'a, I>(mut siblings: I, must_be_upper_case: bool) -> bool
		// 			where I: Iterator<Item = &'a ChildOfElement<'a>> {				
		// 		let mut found_match = false;		// guard against no siblings
		// 		while let Some(child) = siblings.next() {
		// 			let mut maybe_roman_numeral = as_element(*child);
		// 			if name(&maybe_roman_numeral) == "mo" {
		// 				let after_mo = siblings.next();
		// 				if after_mo.is_none() {
		// 					return false;
		// 				}
		// 				maybe_roman_numeral = as_element(*after_mo.unwrap());
		// 			}
		// 			if !is_leaf(maybe_roman_numeral) {
		// 				return false;
		// 			}
		// 			let text = as_text(maybe_roman_numeral);
		// 			if text.trim().is_empty() {
		// 				return false;
		// 			}
		// 			if !(( must_be_upper_case && UPPER_ROMAN_NUMERAL.is_match(text)) ||
		// 				 (!must_be_upper_case && LOWER_ROMAN_NUMERAL.is_match(text)) ) {
		// 					return false;
		// 			};
		// 			found_match = true;
		// 		}
		// 		return found_match;
		// 	}
		// }

		fn is_digit_block(mathml: Element) -> DigitBlockType {
			// returns true if an 'mn' with exactly three digits
			lazy_static! {
				static ref IS_DIGIT_BLOCK_3: Regex = Regex::new(r"^\d\d\d$").unwrap();
				static ref IS_DIGIT_BLOCK_4: Regex = Regex::new(r"^\d\d\d\d$").unwrap();
				static ref IS_DIGIT_BLOCK_5: Regex = Regex::new(r"^\d\d\d\d\d$").unwrap();
				static ref IS_BINARY_DIGIT_BLOCK: Regex = Regex::new(r"^[01]{4}$").unwrap();
			}
			if name(&mathml) == "mn"  {
				let text = as_text(mathml);
				match text.len() {
					3 => if IS_DIGIT_BLOCK_3.is_match(text) {
						return DigitBlockType::DecimalBlock_3;
					},
					4 => if IS_DIGIT_BLOCK_4.is_match(text) {
						return DigitBlockType::DecimalBlock_4;
					} else if IS_BINARY_DIGIT_BLOCK.is_match(text) {
						return DigitBlockType::BinaryBlock_4;
					},
					5 => if IS_DIGIT_BLOCK_5.is_match(text) {
						return DigitBlockType::DecimalBlock_5;
					},
					_ =>  return DigitBlockType::None,
				}
			}
			return DigitBlockType::None;
		}

		/// Merge mtext that is whitespace onto preceding or following mi/mn.
		/// 
		/// Note: this should be called *after* the mo/mtext cleanup (i.e., after the MathML child cleanup loop).
		fn merge_whitespace(children: &mut Vec<ChildOfElement>) {
			let mut i = 0;
			while i < children.len() {
				let child = as_element(children[i]);
				// if we encounter mtext and it is whitespace, it should be normalized to a non-breaking space.
				if name(&child) == "mtext" && as_text(child) == "\u{A0}" {
					// normalize whitespace to just non-breaking space
					// the best merge would be with adjacent mtext (the space might be in 'mo')
					if i < children.len()-1 {
						let next_child = as_element(children[i+1]);
						if name(&next_child) == "mtext"{
							if as_text(next_child) != "\u{A0}" {
								let new_text = "\u{A0}".to_string() + as_text(next_child);
								next_child.set_text(&new_text);
							}
							children.remove(i);	
							continue;	// try again with 'next' removed
						}
					}
					// try to merge with previous
					if i > 0 {
						let prev_child = as_element(children[i-1]);
						if name(&prev_child) == "mi" || name(&prev_child) == "mn" || name(&prev_child) == "mtext" {
							let new_text = as_text(prev_child).to_string() + "\u{A0}";
							prev_child.set_text(&new_text);
							children.remove(i);
							continue;		// don't advance 'i'
						}	
					}
					if i < children.len()-1 {	// try to merge with next
						let next_child = as_element(children[i+1]);
						if name(&next_child) == "mi" || name(&next_child) == "mn" {
							let new_text = "\u{A0}".to_string() + as_text(next_child);
							next_child.set_text(&new_text);
							children.remove(i);
							i += 1; 	// don't need to look at next child since we know what it is
							continue;
						}
					}
				}
				i += 1;
			}
		}

		/// look for potential numbers by looking for sequences with commas, spaces, and decimal points
		fn merge_number_blocks(parent_mrow: Element, children: &mut Vec<ChildOfElement>) {
			lazy_static!{
				static ref SEPARATORS: Regex = Regex::new(r"[],. \u{00A0}]").unwrap(); 
			}
			// debug!("parent:\n{}", mml_to_string(&parent_mrow));
			let mut i = 0;
			while i < children.len() {
				let child = as_element(children[i]);
				let mut is_comma = false;
				let mut is_decimal_pt = false;
				let mut has_decimal_pt = false;
				if name(&child) == "mn" {
					// if the 'mn' has ',', '.', or space, consider it correctly parsed and move on
					if SEPARATORS.is_match(as_text(child)) {
						i += 1;
						continue;
					}

					// potential start of a number
					let mut start = i;
					let mut looking_for_separator = true;
					if i > 0 && name(&as_element(children[i-1])) == "mo" {
						let leaf_text = as_text(as_element(children[i-1]));
						is_comma = leaf_text == ",";
						is_decimal_pt = leaf_text == ".";
						has_decimal_pt = is_decimal_pt;
						if is_decimal_pt {
							start = i - 1;
						}
					}
	
					let mut end = children.len();
					for (j, sibling) in children[i+1..].iter().enumerate() {
						let sibling = as_element(*sibling);
						let sibling_name = name(&sibling);
						if sibling_name != "mn" {
							if sibling_name=="mo" || sibling_name=="mtext" {
								// FIX: generalize to include locale ("." vs ",")
								let leaf_text = as_text(sibling);
								if !(leaf_text=="." || leaf_text=="," || leaf_text.trim().is_empty()) || 
								   (leaf_text=="." && has_decimal_pt) {
									end = start + j+1;
									break;
								} else if looking_for_separator {
									is_comma = leaf_text == ",";
									is_decimal_pt = leaf_text == ".";
								} else {
									is_comma = false;
									is_decimal_pt = false;
								}
							} else {
								end = start + j+1;
								break;
							}
						}
						// debug!("j/name={}/{}, looking={}, is ',' {}, '.' {}, ",
						// 		 i+j, sibling_name, looking_for_separator, is_comma, is_decimal_pt);
						if !(looking_for_separator &&
							 (sibling_name == "mtext" || is_comma || is_decimal_pt)) &&
						   ( looking_for_separator ||
						   	 !(is_decimal_pt || is_digit_block(sibling) != DigitBlockType::None)) {
							end = start + if is_decimal_pt {j+2} else {j+1};
							break;
						}
						looking_for_separator = !looking_for_separator;
					}
					// debug!("start={}, end={}", start, end);
					if is_likely_a_number(parent_mrow, children, start, end) {
						merge_block(children, start, end);
						// note: i..i+end has been collapsed, so just inc 'i' by one
					} else {
						i = end-1;	// start looking at the end of the block we just rejected
					}
				}
				i += 1;
			}
		}

		/// If we have something like 'shape' ABC, we split the ABC and add IMPLIED_SEPARATOR_HIGH_PRIORITY between them
		/// under some specific conditions (trying to be a little cautious).
		/// The returned (mrow) element reuses the arg so tree siblings links remain correct.
		fn split_points(leaf: Element) -> Option<Element> {
			lazy_static!{
				static ref IS_UPPERCASE: Regex = Regex::new(r"^[A-Z]+$").unwrap(); 
			}

			if !IS_UPPERCASE.is_match(as_text(leaf)) {
				return None;
			}

			// check to see if there is a bar, arrow, etc over the letters (line-segment, arc, ...)
			let parent = leaf.parent().unwrap().element().unwrap();
			if name(&parent) == "mover" {
				// look for likely overscripts (basically just rule out some definite 'no's)
				let over = as_element(parent.children()[1]);
				if is_leaf(over) {
					let mut over_chars = as_text(over).chars();
					let first_char = over_chars.next();
					if first_char.is_some() && over_chars.next().is_none() && !first_char.unwrap().is_alphanumeric(){
						// only one char and it isn't alphanumeric
						return Some( split_element(leaf) );
					}
				}
			}
	
			// check to see if it is preceded by a geometric shape (e.g, ∠ABC)
			let preceding_siblings = leaf.preceding_siblings();
			if !preceding_siblings.is_empty() {
				let preceding_sibling = as_element(preceding_siblings[preceding_siblings.len()-1]);
				let preceding_sibling_name = name(&preceding_sibling);
				if preceding_sibling_name == "mi" || preceding_sibling_name == "mo" || preceding_sibling_name == "mtext" {
					let preceding_text = as_text(preceding_sibling);
					return crate::definitions::DEFINITIONS.with(|definitions| {
						let defs = definitions.borrow();
						let prefix_ops = defs.get_hashset("GeometryPrefixOperators").unwrap();
						let shapes = defs.get_hashset("GeometryShapes").unwrap();
						if prefix_ops.contains(preceding_text) || shapes.contains(preceding_text) {
							// split leaf
							return Some( split_element(leaf) );	// always treated as function names
						} else {
							return None;
						}
					})
				}
			}
			return None;

			fn split_element(leaf: Element) -> Element {
				let mut children = Vec::with_capacity(leaf.children().len());
				for ch in as_text(leaf).chars() {
					let new_leaf = create_mathml_element(&leaf.document(), "mi");
					new_leaf.set_text(&ch.to_string());
					children.push(new_leaf);
				}
				set_mathml_name(leaf, "mrow");
				leaf.replace_children(children);
				return leaf;
			}
		}


		fn is_likely_a_number(mrow: Element, children: &[ChildOfElement], mut start: usize, mut end: usize) -> bool {
			if count_decimal_pts(children, start, end) > 1 {
				return false;
			}

			// remove/don't include whitespace at the end
			while end >= start+3 {
				let child = as_element(children[end-1]);	// end is not inclusive
				if !is_leaf(child) || !as_text(child).trim().is_empty() {
					break;
				}
				end -= 1;
			}

			let decimal_at_start = count_decimal_pts(children, start, start+1) == 1;
			// decimal_at_start => none at end
			let decimal_at_end = !(decimal_at_start || count_decimal_pts(children, end-1, end) == 0);
			// be a little careful about merging the numbers	
			if end - start < 3 {
				// need at least digit separator digit-block unless it starts or ends with a decimal point
				return decimal_at_start || decimal_at_end;
			}

			// simplify a little by removing starting/ending decimals
			if decimal_at_start {
				start += 1;
			} else if decimal_at_end {
				end -= 1;
			}

			if name(&as_element(children[end-1])) != "mn" {
				return false;		// end with a digit block (always starts with a number)
			}

			if name(&as_element(children[start+1])) == "mtext" || 
			   IS_WHITESPACE.is_match(as_text(as_element(children[start+1]))) {
			    // make sure all the digit blocks are of the same type
				let mut digit_block = DigitBlockType::None;		// initial "illegal" value (we know it is not NONE)
				for &child in children {
					let child = as_element(child);
					if name(&child) == "mn" {
						if digit_block == DigitBlockType::None {
							digit_block = is_digit_block(child);
						} else if is_digit_block(child) != digit_block {
							return false;		// differing digit block types
						}
					}
				}
				return true;		// digit block separated by whitespace
			}

			// if we have 1,23,456 we don't want to consider 23,456 a number
			// so we check in front of 23,456 for d,
			// we don't need to check the symmetric case '1,234,56' because calling logic won't flag this as a potential number
			if start > 1 && name(&as_element(children[0])) == "mn" {
				let potential_comma = as_element(children[1]);
				if name(&potential_comma) == "mo" && as_text(potential_comma) == "," {
					return false;
				}
			}

			// If surrounded by fences, and commas are used, leave as is (e.g, "{1,234}")
			// We have already checked for whitespace as separators, so it must be a comma. Just check the fences.
			// This is not yet in canonical form, so the fences may be siblings or siblings of the parent 
			let first_child;
			let last_child;
			if start == 0 && end == children.len() {
				let preceding_children = mrow.preceding_siblings();
				let following_children = mrow.following_siblings();
				if preceding_children.is_empty() || following_children.is_empty() {
					return true;	// doesn't have left or right fence
				}
				first_child = preceding_children[preceding_children.len()-1];
				last_child = following_children[0];
			} else if start > 0 && end < children.len() {
				first_child = children[start-1];
				last_child = children[end];
			} else {
				return true; // can't be fences around it
			}
			let first_child = as_element(first_child);
			let last_child = as_element(last_child);
			// debug!("first_child: {}", crate::pretty_print::mml_to_string(&first_child));
			// debug!("last_child: {}", crate::pretty_print::mml_to_string(&last_child));
			return !(name(&first_child) == "mo" && is_fence(first_child) &&
				     name(&last_child) == "mo" && is_fence(last_child) );
		}

		fn count_decimal_pts(children: &[ChildOfElement], start: usize, end: usize) -> usize {
			let mut n_decimal_pt = 0;
			for &child_as_element in children.iter().take(end).skip(start) {
				let child = as_element(child_as_element);
				if as_text(child).contains('.')  {
					n_decimal_pt += 1;
				}
			}
			return n_decimal_pt;
		}

		fn merge_block(children: &mut Vec<ChildOfElement>, start: usize, end: usize) {
			let mut mn_text = String::with_capacity(4*(end-start)-1);		// true size less than #3 digit blocks + separator
			for &child_as_element in children.iter().take(end).skip(start) {
				let child = as_element(child_as_element);
				mn_text.push_str(as_text(child));
			}
			let child = as_element(children[start]);
			set_mathml_name(child, "mn");
			child.set_text(&mn_text);

			children.drain(start+1..end);
		}

		fn merge_dots(mrow: Element) -> Element {
			// merge consecutive <mo>s containing '.' into ellipsis
			let children = mrow.children();
			let mut i = 0;
			let mut n_dots = 0;		// number of consecutive mo's containing dots
			while i < children.len() {
				let child = as_element(children[i]);
				if name(&child) == "mo" {
					let text = as_text(child);
					if text == "." {
						n_dots += 1;
						if n_dots == 3 {
							let first_child = as_element(children[i-2]);
							first_child.set_text("…");
							as_element(children[i-1]).remove_from_parent();
							child.remove_from_parent();
							n_dots = 0;
						}
					} else {
						n_dots = 0;
					}
				} else {
					n_dots = 0;
				}
				i += 1;
			}
			return mrow;
		}

		fn merge_primes(mrow: Element) -> Element {
			// merge consecutive <mo>s containing primes (in various forms)
			let mut children = mrow.children();
			let mut i = 0;
			let mut n_primes = 0;		// number of consecutive mo's containing primes
			while i < children.len() {
				let child = as_element(children[i]);
				if name(&child) == "mo" {
					let text = as_text(child);
					// FIX: should we be more restrictive and change (apostrophe) only in a superscript?
					if IS_PRIME.is_match(text) {
						n_primes += 1;
					} else if n_primes > 0 {
						merge_prime_elements(&mut children, i - n_primes, i);
						n_primes = 0;
					}
				} else if n_primes > 0 {
					merge_prime_elements(&mut children, i - n_primes, i);
					n_primes = 0;
				}
				i += 1;
			}
			if n_primes > 0 {
				merge_prime_elements(&mut children, i - n_primes, i);
			}
			return mrow;
		}

		fn merge_prime_elements(children: &mut [ChildOfElement], start: usize, end: usize) {
			// not very efficient since this is probably causing an array shift each time (array is probably not big though)
			let first_child = as_element(children[start]);
			let mut new_text = String::with_capacity(end+3-start);	// one per element plus a little extra
			new_text.push_str(as_text(first_child));
			for &child_as_element in children.iter().take(end).skip(start+1) {
				let child = as_element(child_as_element);
				let text = as_text(child); 		// only in this function because it is an <mo>
				new_text.push_str(text);
				child.remove_from_parent();
			}
			first_child.set_text(&merge_prime_text(&new_text));
		}
	
		fn merge_prime_text(text: &str) -> String {
			// merge together single primes into double primes, etc.
			let mut n_primes = 0;
			for ch in text.chars() {
				match ch {
					'\'' | '′' => n_primes += 1,
					'″' => n_primes += 2,
					'‴' => n_primes += 3,
					'⁗' => n_primes += 4,
					_ => {
						eprint!("merge_prime_text: unexpected char '{}' found", ch);
						return text.to_string();
					}
				}
			}
			// it would be very rare to have more than a quadruple prime, so the inefficiency in the won't likely happen
			let mut result = String::with_capacity(n_primes);	// likely 4x too big, but string is short-lived and small
			for _ in 0..n_primes/4 {
				result.push('⁗');
			}
			match n_primes % 4 {
				1 => result.push('′'),
				2 => result.push('″'),
				3 => result.push('‴'),
				_ => ()	// can't happen
			}
			return result;
		}

		fn handle_pseudo_scripts(mrow: Element) -> Element {
			// from https://www.w3.org/TR/MathML3/chapter7.html#chars.pseudo-scripts
			static PSEUDO_SCRIPTS: phf::Set<&str> = phf_set! {
				"\"", "'", "*", "`", "ª", "°", "²", "³", "´", "¹", "º",
				"‘", "’", "“", "”", "„", "‟",
				"′", "″", "‴", "‵", "‶", "‷", "⁗",
			};
	
			// merge consecutive <mo>s containing primes (in various forms)
			let mut children = mrow.children();
			let mut i = 1;
			let mut found = false;
			while i < children.len() {
				let child = as_element(children[i]);
				if name(&child) == "mo" && PSEUDO_SCRIPTS.contains(as_text(child)) {
					let msup = create_mathml_element(&child.document(), "msup");
					msup.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
					msup.append_child(children[i-1]);
					msup.append_child(child);
					children[i-1] = ChildOfElement::Element(msup);
					children.remove(i);
					found = true;
				} else {
					i += 1;
				}
			}
			if found {
				mrow.replace_children(children)
			}
			return mrow;
		}

		fn handle_convert_to_mmultiscripts(children: &mut Vec<ChildOfElement>) {
  			let mut i = 0;
			while i < children.len() {
				let child = as_element(children[i]);
				let child_name = name(&child);
				if (child_name == "msub" || child_name == "msup" || child_name == "msubsup") && CanonicalizeContext::is_empty_element(as_element(child.children()[0])) {
					i = convert_to_mmultiscripts(children, i);
				} else {
					i += 1;
				}
			}
		}


		/// Converts the script element with an empty base to mmultiscripts by sucking the base from the following or preceding element.
		/// The following element is preferred so that these become prescripts (common usage is from TeX), but if the preceding element
		///   has a closer mi/mtext, it is used.
		/// mhchem has some ugly output (at least in MathJax) and that's where using the following element makes sense
		///   because an empty based (mpadded width=0) is used for the scripts. A hacky attribute indicates this case.
		fn convert_to_mmultiscripts(mrow_children: &mut Vec<ChildOfElement>, i: usize) -> usize {
			// this is a bit messy/confusing because we might scan forwards or backwards and this affects whether
			// we are scanning for prescripts or postscripts
			// the generic name "primary_scripts" means prescripts if going forward or postscripts if going backwards
			// if we are going forward and hit a sub/superscript with a base, then those scripts become postscripts ("other_scripts")
			// if we are going backwards, we never add prescripts

			// let parent = as_element(mrow_children[i]).parent().unwrap().element().unwrap();
			// debug!("convert_to_mmultiscripts (i={}) -- PARENT:\n{}", i, mml_to_string(&parent));

			let i_base = choose_base_of_mmultiscripts(mrow_children, i);
			let mut base = as_element(mrow_children[i_base]);
			// debug!("convert_to_mmultiscripts -- base\n{}", mml_to_string(&base));
			let base_name = name(&base);
			let mut prescripts = vec![];
			let mut postscripts = vec![];
			let mut i_postscript = i_base + 1;

			if (base_name == "msub" || base_name == "msup" || base_name == "msubsup") &&
			   !CanonicalizeContext::is_empty_element(as_element(base.children()[0])) {
				// if the base is a script element, then we want the base of that to be the base of the mmultiscripts
				let mut base_children = base.children();
				let script_base = as_element(base.children()[0]);
				base_children[0] = ChildOfElement::Element(CanonicalizeContext::create_empty_element(&base.document()));
				base.replace_children(base_children);
				add_to_scripts(base, &mut postscripts);
				base = script_base;
			}

			if i_base > i {
				// we have prescripts -- gather them up
				let mut i_prescript = i;
				while i_prescript < i_base {
					let script = as_element(mrow_children[i_prescript]);
					if !add_to_scripts(script, &mut prescripts) {
						break;
					}
					i_prescript += 1;
				}
			}

			// gather up the postscripts (if any)
			while i_postscript < mrow_children.len() {
				let script = as_element(mrow_children[i_postscript]);
				if name(&script) == "msub" && i_postscript+1 < mrow_children.len() {
					let superscript = as_element(mrow_children[i_postscript+1]);
					if name(&superscript) == "msup" && CanonicalizeContext::is_empty_element(as_element(superscript.children()[0])) {
						set_mathml_name(script, "msubsup");
						script.append_child(superscript.children()[1]);
						i_postscript += 1;
					}
				}
				// debug!("adding script\n{}", mml_to_string(&script));
				if !add_to_scripts(script, &mut postscripts) {
					break;
				}
				i_postscript += 1;
			}

			let i_returned = if i_base < i {i_base} else {i};
			let script = create_mathml_element(&base.document(), "mmultiscripts");
			let mut num_children = 1 + postscripts.len();
			if !prescripts.is_empty() {
				num_children += 1 + prescripts.len();
			}
			let mut new_children = Vec::with_capacity(num_children);
			new_children.push(ChildOfElement::Element(base));
			new_children.append(&mut postscripts);
			if !prescripts.is_empty() {
				new_children.push( ChildOfElement::Element( create_mathml_element(&script.document(), "mprescripts") ) );
				new_children.append(&mut prescripts);
			}

			script.replace_children(new_children);
			mrow_children[i_returned] = ChildOfElement::Element(script);
			mrow_children.drain(i_returned+1..i_postscript);	// remove children after the first

			let likely_chemistry = likely_adorned_chem_formula(script);
			if likely_chemistry >= 0 {
				script.set_attribute_value(MAYBE_CHEMISTRY, likely_chemistry.to_string().as_str());
			}

			// debug!("convert_to_mmultiscripts -- converted script:\n{}", mml_to_string(&script));
			return i_returned;
		}

		fn add_to_scripts<'a>(el: Element<'a>, scripts: &mut Vec<ChildOfElement<'a>>) -> bool {
			let script_name = name(&el);
			if !(script_name == "msub" || script_name == "msup" || script_name == "msubsup") ||
			   !CanonicalizeContext::is_empty_element(as_element(el.children()[0])) {
					return false;
			}
			if script_name == "msub" {
				add_pair(scripts, Some(el.children()[1]), None);
			} else if script_name == "msup" {
				add_pair(scripts, None, Some(el.children()[1]));
			} else { // msubsup
				add_pair(scripts, Some(el.children()[1]), Some(el.children()[2]));
			};
			return true;
		}

		fn add_pair<'v, 'a:'v>(script_vec: &'v mut Vec<ChildOfElement<'a>>, subscript: Option<ChildOfElement<'a>>, superscript: Option<ChildOfElement<'a>>) {
			let child_of_element = if let Some(subscript) = subscript {subscript} else {superscript.unwrap()};
			let doc = as_element(child_of_element).document();
			let subscript = if let Some(subscript)= subscript {
				if CanonicalizeContext::is_empty_element(as_element(subscript)) {
					ChildOfElement::Element(create_mathml_element(&doc, "none"))
				} else {
					subscript
				}
			} else {
				ChildOfElement::Element(create_mathml_element(&doc, "none"))
			};
			let superscript = if let Some(superscript) = superscript {
				if CanonicalizeContext::is_empty_element(as_element(superscript)) {
					ChildOfElement::Element(create_mathml_element(&doc, "none"))
				} else {
					superscript
				}
			} else {
				ChildOfElement::Element(create_mathml_element(&doc, "none"))
			};
			script_vec.push(subscript);
			script_vec.push(superscript);
		}

		/// Find the closest likely base to the 'i'th child, preferring the next one over the preceding one, but want the closest.
		///
		/// Note: because the base might be (...), 'mrow_children might be changed so that they are grouped into an mrow.
		fn choose_base_of_mmultiscripts(mrow_children: &mut Vec<ChildOfElement>, i: usize) -> usize {
			// We already know there are no empty scripts to the left (because we find first empty base from left to right).
			// However, there may be some empty bases before we get to real base on the right.
			let script_element_base = as_element(as_element(mrow_children[i]).children()[0]);
			let from_mchem = script_element_base.attribute(MHCHEM_MMULTISCRIPTS_HACK).is_some();
			if mrow_children.len() > i+1 && !(from_mchem && i > 0) && is_child_simple_base(mrow_children[i+1]) {
				return i+1;
			}
			if i > 0 {
				if let Some(i_start) = is_grouped_base(&mrow_children[..i]) {
					assert!(i_start < i-1);	// should be at least two children (open and close)
					// create a new mrow, add the grouped children to it, then drain all but the first of them from the original mrow vec.
					// stick the mrow into the first of them -- this is the base
					let new_mrow = create_mathml_element(&as_element(mrow_children[0]).document(), "mrow");
					new_mrow.set_attribute_value(CHANGED_ATTR, ADDED_ATTR_VALUE);
					for &child in &mrow_children[i_start..i] {
						new_mrow.append_child(child);
					}
					mrow_children.drain(i_start+1..i);
					mrow_children[i_start] = ChildOfElement::Element(new_mrow);
					return i_start;
				}
				if is_child_simple_base(mrow_children[i-1]) {
					return i-1;
				}
			}

			// base very likely after multiple scripts to the right
			for i_base in i+1..mrow_children.len() {
				if is_child_simple_base(mrow_children[i_base]) {
						return i_base;
				} else {
					let child = as_element(mrow_children[i_base]);
					let child_name = name(&child);
					if !(child_name == "msub" || child_name == "msup" || child_name == "msubsup") {
						break;
					}
				}
			}
			// didn't find any good candidates for a base -- pick something valid
			assert!(mrow_children.len() > i);
			return i;
			
			
			fn is_child_simple_base(child: ChildOfElement) -> bool {
				let mut child = as_element(child);
				let child_name = name(&child);
				if child_name == "msub" || child_name == "msup" || child_name == "msubsup" {
					child = as_element(child.children()[0]);
				}

				return is_leaf(child) && !CanonicalizeContext::is_empty_element(child);  // a little overly general (but hopefully doesn't matter)
			}

			/// Return the index of the matched open paren/bracket if the last element is a closed paren/bracket
			fn is_grouped_base(mrow_children: &[ChildOfElement]) -> Option<usize> {
				// FIX: this really belongs in canonicalization pass, not the clean pass
				let i_last = mrow_children.len()-1;
				let last_child = as_element(mrow_children[i_last]);
				if name(&last_child) == "mo" &&
				   CanonicalizeContext::new().find_operator(last_child, None, None, None).is_right_fence() {
					for i_child in (0..i_last).rev() {
						let child = as_element(mrow_children[i_child]);
						if name(&child) == "mo" &&
						   CanonicalizeContext::new().find_operator(child, None, None, None).is_left_fence() {
							// FIX: should make sure left and right match. Should also count for nested parens
							return Some(i_child);
						}
					}
				}
				return None;
			}
		}
	}

	fn canonicalize_mrows<'a>(&self, mathml: Element<'a>) -> Result<Element<'a>> {
		let tag_name = name(&mathml);
		set_mathml_name(mathml, tag_name);	// add namespace
		match tag_name {
			"mi" | "ms" | "mtext" | "mspace"  => {
				self.canonicalize_plane1(mathml);
				return Ok( mathml ); },
			"mo" => {
				self.canonicalize_plane1(mathml);
				self.canonicalize_mo_text(mathml);
				return Ok( mathml );
			},
			"mn" => {
				self.canonicalize_plane1(mathml);
				return Ok( mathml );
			},
			"mrow" => {
				return self.canonicalize_mrows_in_mrow(mathml);
			},
			"semantics" => {
				let mut children = mathml.children();
				let (i, presentation) = get_presentation_element(mathml);
				children[i] = ChildOfElement::Element(self.canonicalize_mrows(presentation)? );
				mathml.replace_children(children);
				return Ok(mathml);
			},
			_ => {
				// recursively try to make mrows in other structures (eg, num/denom in fraction)
				let mut new_children = Vec::with_capacity(mathml.children().len());
				for child in mathml.children() {
					match child {
						ChildOfElement::Element(e) => {
							new_children.push( ChildOfElement::Element(self.canonicalize_mrows(e)? ));
						},
						_ => panic!("Should have been an element or text"),
					}
				}
				mathml.replace_children(new_children);
				return Ok( mathml );
			},
		}
	}
		
	fn potentially_lift_script<'a>(&self, mrow: Element<'a>) -> Element<'a> {
		if name(&mrow) != "mrow" {
			return mrow;
		}
		let mut mrow_children = mrow.children();
		let first_child = as_element(mrow_children[0]);
		let last_child = as_element(mrow_children[mrow_children.len()-1]);
		let last_child_name = name(&last_child);

		if name(&first_child) == "mo" && is_fence(first_child) &&
		   (last_child_name == "msub" || last_child_name == "msup" || last_child_name == "msubsup") {
			let base = as_element(last_child.children()[0]);
			if !(name(&base) == "mo" && is_fence(base)) {
				return mrow;	// not a case we are interested in
			}
			// else drop through
		} else {
			return mrow; // not a case we are interested in
		}

		let script = last_child;	// better name now that we know what it is
		let mut script_children = script.children();
		let close_fence = script_children[0];
		let mrow_children_len = mrow_children.len();		  // rust complains about a borrow after move if we don't store this first
		mrow_children[mrow_children_len-1] = close_fence;		  // make the mrow hold the fences
		mrow.replace_children(mrow_children);
		// make the mrow the child of the script
		script_children[0] = ChildOfElement::Element(mrow);
		script.replace_children(script_children);
		return script;
	}

	fn canonicalize_plane1<'a>(&self, mi: Element<'a>) -> Element<'a> {
		// map names to start of Unicode alphanumeric blocks (Roman, digits, Greek)
		// if the character shouldn't be mapped, use 0 -- don't use 'A' as ASCII and Greek aren't contiguous
		static MATH_VARIANTS: phf::Map<&str, [u32; 3]> = phf_map! {
			// "normal" -- nothing to do
			"italic" => [0, 0, 0x1D6E2],
			"bold" => [0x1D400, 0x1D7CE, 0x1D6A8],
			"bold-italic" => [0x1D468, 0x1D7CE, 0x1D71C],
			"double-struck" => [0x1D538, 0x1D7D8, 0],
			"bold-fraktur" => [0x1D56C, 0, 0x1D6A8],
			"script" => [0x1D49C, 0, 0],
			"bold-script" => [0x1D4D0, 0, 0x1D6A8],
			"fraktur" => [0x1D504, 0, 0],
			"sans-serif" => [0x1D5A0, 0x1D7E2, 0],
			"bold-sans-serif" => [0x1D5D4, 0x1D7EC, 0x1D756],
			"sans-serif-italic" => [0x1D608, 0x1D7E2, 0],
			"sans-serif-bold-italic" => [0x1D63C, 0x1D7EC, 0x1D790],
			"monospace" => [0x1D670, 0x1D7F6, 0],
		};

		let variant = mi.attribute_value("mathvariant");
		if variant.is_none() {
			return mi;
		}

		let mi_text = as_text(mi);
		let new_text = match MATH_VARIANTS.get(variant.unwrap()) {
			None => mi_text.to_string(),
			Some(start) => shift_text(mi_text, start),
		};
		// mi.remove_attribute("mathvariant");  // leave attr -- for Nemeth, there are italic digits etc that don't have Unicode points
		mi.set_text(&new_text);
		return mi;

		fn shift_text(old_text: &str, char_mapping: &[u32; 3]) -> String {
			// if there is no block for something, use 'a', 'A', 0 as that will be a no-op
			struct Offsets {
				ch: u32,
				table: usize, 
			}
			static SHIFT_AMOUNTS: phf::Map<char, Offsets> = phf_map! {
				'A' => Offsets{ ch: 0, table: 0},
				'B' => Offsets{ ch: 1, table: 0},
				'C' => Offsets{ ch: 2, table: 0},
				'D' => Offsets{ ch: 3, table: 0},
				'E' => Offsets{ ch: 4, table: 0},
				'F' => Offsets{ ch: 5, table: 0},
				'G' => Offsets{ ch: 6, table: 0},
				'H' => Offsets{ ch: 7, table: 0},
				'I' => Offsets{ ch: 8, table: 0},
				'J' => Offsets{ ch: 9, table: 0},
				'K' => Offsets{ ch: 10, table: 0},
				'L' => Offsets{ ch: 11, table: 0},
				'M' => Offsets{ ch: 12, table: 0},
				'N' => Offsets{ ch: 13, table: 0},
				'O' => Offsets{ ch: 14, table: 0},
				'P' => Offsets{ ch: 15, table: 0},
				'Q' => Offsets{ ch: 16, table: 0},
				'R' => Offsets{ ch: 17, table: 0},
				'S' => Offsets{ ch: 18, table: 0},
				'T' => Offsets{ ch: 19, table: 0},
				'U' => Offsets{ ch: 20, table: 0},
				'V' => Offsets{ ch: 21, table: 0},
				'W' => Offsets{ ch: 22, table: 0},
				'X' => Offsets{ ch: 23, table: 0},
				'Y' => Offsets{ ch: 24, table: 0},
				'Z' => Offsets{ ch: 25, table: 0},
				'a' => Offsets{ ch: 26, table: 0},
				'b' => Offsets{ ch: 27, table: 0},
				'c' => Offsets{ ch: 28, table: 0},
				'd' => Offsets{ ch: 29, table: 0},
				'e' => Offsets{ ch: 30, table: 0},
				'f' => Offsets{ ch: 31, table: 0},
				'g' => Offsets{ ch: 32, table: 0},
				'h' => Offsets{ ch: 33, table: 0},
				'i' => Offsets{ ch: 34, table: 0},
				'j' => Offsets{ ch: 35, table: 0},
				'k' => Offsets{ ch: 36, table: 0},
				'l' => Offsets{ ch: 37, table: 0},
				'm' => Offsets{ ch: 38, table: 0},
				'n' => Offsets{ ch: 39, table: 0},
				'o' => Offsets{ ch: 40, table: 0},
				'p' => Offsets{ ch: 41, table: 0},
				'q' => Offsets{ ch: 42, table: 0},
				'r' => Offsets{ ch: 43, table: 0},
				's' => Offsets{ ch: 44, table: 0},
				't' => Offsets{ ch: 45, table: 0},
				'u' => Offsets{ ch: 46, table: 0},
				'v' => Offsets{ ch: 47, table: 0},
				'w' => Offsets{ ch: 48, table: 0},
				'x' => Offsets{ ch: 49, table: 0},
				'y' => Offsets{ ch: 50, table: 0},
				'z' => Offsets{ ch: 51, table: 0},
				'0' => Offsets{ ch: 0, table: 1},
				'1' => Offsets{ ch: 1, table: 1},
				'2' => Offsets{ ch: 2, table: 1},
				'3' => Offsets{ ch: 3, table: 1},
				'4' => Offsets{ ch: 4, table: 1},
				'5' => Offsets{ ch: 5, table: 1},
				'6' => Offsets{ ch: 6, table: 1},
				'7' => Offsets{ ch: 7, table: 1},
				'8' => Offsets{ ch: 8, table: 1},
				'9' => Offsets{ ch: 9, table: 1},
				'Α' => Offsets{ ch: 0, table: 2},
				'Β' => Offsets{ ch: 1, table: 2},
				'Γ' => Offsets{ ch: 2, table: 2},
				'Δ' => Offsets{ ch: 3, table: 2},
				'Ε' => Offsets{ ch: 4, table: 2},
				'Ζ' => Offsets{ ch: 5, table: 2},
				'Η' => Offsets{ ch: 6, table: 2},
				'Θ' => Offsets{ ch: 7, table: 2},
				'Ι' => Offsets{ ch: 8, table: 2},
				'Κ' => Offsets{ ch: 9, table: 2},
				'Λ' => Offsets{ ch: 10, table: 2},
				'Μ' => Offsets{ ch: 11, table: 2},
				'Ν' => Offsets{ ch: 12, table: 2},
				'Ξ' => Offsets{ ch: 13, table: 2},
				'Ο' => Offsets{ ch: 14, table: 2},
				'Π' => Offsets{ ch: 15, table: 2},
				'Ρ' => Offsets{ ch: 16, table: 2},
				'ϴ' => Offsets{ ch: 17, table: 2},
				'Σ' => Offsets{ ch: 18, table: 2},
				'Τ' => Offsets{ ch: 19, table: 2},
				'Υ' => Offsets{ ch: 20, table: 2},
				'Φ' => Offsets{ ch: 21, table: 2},
				'Χ' => Offsets{ ch: 22, table: 2},
				'Ψ' => Offsets{ ch: 23, table: 2},
				'Ω' => Offsets{ ch: 24, table: 2},
				'∇' => Offsets{ ch: 25, table: 2},								
				'α' => Offsets{ ch: 26, table: 2},
				'β' => Offsets{ ch: 27, table: 2},
				'γ' => Offsets{ ch: 28, table: 2},
				'δ' => Offsets{ ch: 29, table: 2},
				'ε' => Offsets{ ch: 30, table: 2},
				'ζ' => Offsets{ ch: 31, table: 2},
				'η' => Offsets{ ch: 32, table: 2},
				'θ' => Offsets{ ch: 33, table: 2},
				'ι' => Offsets{ ch: 34, table: 2},
				'κ' => Offsets{ ch: 35, table: 2},
				'λ' => Offsets{ ch: 36, table: 2},
				'μ' => Offsets{ ch: 37, table: 2},
				'ν' => Offsets{ ch: 38, table: 2},
				'ξ' => Offsets{ ch: 39, table: 2},
				'ο' => Offsets{ ch: 40, table: 2},
				'π' => Offsets{ ch: 41, table: 2},
				'ρ' => Offsets{ ch: 42, table: 2},
				'ς' => Offsets{ ch: 43, table: 2},
				'σ' => Offsets{ ch: 44, table: 2},
				'τ' => Offsets{ ch: 45, table: 2},
				'υ' => Offsets{ ch: 46, table: 2},
				'φ' => Offsets{ ch: 47, table: 2},
				'χ' => Offsets{ ch: 48, table: 2},
				'ψ' => Offsets{ ch: 49, table: 2},
				'ω' => Offsets{ ch: 50, table: 2},
				'∂' => Offsets{ ch: 51, table: 2},
				'ϵ' => Offsets{ ch: 52, table: 2},
				'ϑ' => Offsets{ ch: 53, table: 2},
				'ϰ' => Offsets{ ch: 54, table: 2},
				'ϕ' => Offsets{ ch: 55, table: 2},
				'ϱ' => Offsets{ ch: 56, table: 2},
				'ϖ' => Offsets{ ch: 57, table: 2},
			};
			let mut new_text = String::new();
			for ch in old_text.chars() {
				new_text.push(
					match SHIFT_AMOUNTS.get(&ch) {
						None => {
							// there are two digamma chars only in the bold mapping. Handled here
							if char_mapping[2] == 0x1D6A8 {
								match ch {
									'Ϝ' => '𝟊',
									'ϝ' => '𝟋',
									_   => ch,
								}
							} else {
								ch
							}
						},
						Some(offsets) => {
							let start_of_mapping = char_mapping[offsets.table];
							if start_of_mapping == 0 {ch} else {shift_char(start_of_mapping + offsets.ch)}
						}
					}
				)
			}
			return new_text;

			fn shift_char(ch: u32) -> char {
				// there are "holes" in the math alphanumerics due to legacy issues
				// this table maps the holes to their legacy location
				static EXCEPTIONS: phf::Map<u32, u32> = phf_map! {
					0x1D455u32 => 0x210Eu32,
					0x1D49Du32 => 0x212Cu32,
					0x1D4A0u32 => 0x2130u32,
					0x1D4A1u32 => 0x2131u32,
					0x1D4A3u32 => 0x210Bu32,
					0x1D4A4u32 => 0x2110u32,
					0x1D4A7u32 => 0x2112u32,
					0x1D4A8u32 => 0x2133u32,
					0x1D4ADu32 => 0x211Bu32,
					0x1D4BAu32 => 0x212Fu32,
					0x1D4BCu32 => 0x210Au32,
					0x1D4C4u32 => 0x2134u32,
					0x1D506u32 => 0x212Du32,
					0x1D50Bu32 => 0x210Cu32,
					0x1D50Cu32 => 0x2111u32,
					0x1D515u32 => 0x211Cu32,
					0x1D51Du32 => 0x2128u32,
					0x1D53Au32 => 0x2102u32,
					0x1D53Fu32 => 0x210Du32,
					0x1D545u32 => 0x2115u32,
					0x1D547u32 => 0x2119u32,
					0x1D548u32 => 0x211Au32,
					0x1D549u32 => 0x211Du32,
					0x1D551u32 => 0x2124u32,
				};
								
				return unsafe { char::from_u32_unchecked(
					match EXCEPTIONS.get(&ch) {
						None => ch,
						Some(exception_value) => *exception_value,
					}
				) }
			}
		}
	}

	fn canonicalize_mo_text(&self, mo: Element) {
		// lazy_static! {
		// 	static ref IS_LIKELY_SCALAR_VARIABLE: Regex = Regex::new("[a-eh-z]").unwrap(); 
		// }
		
		let mut mo_text = as_text(mo);
		let parent = mo.parent().unwrap().element().unwrap();
		let parent_name = name(&parent);
		let is_base = mo.preceding_siblings().is_empty();
		if !is_base && (parent_name == "mover" || parent_name == "munder" || parent_name == "munderover") {
			// canonicalize various diacritics for munder, mover, munderover
			mo_text = match mo_text {
				"_" | "\u{02C9}"| "\u{0304}"| "\u{0305}"| "\u{2212}" |
				"\u{2010}" | "\u{2011}" | "\u{2012}" | "\u{2013}" | "\u{2014}" | "\u{2015}" => "\u{00AF}",
				"\u{02BC}" => "`",
				"\u{02DC}" => "~",
				"\u{02C6}"| "\u{0302}" => "^",
				"\u{0307}" => "\u{02D9}",	// Nemeth distinguishes this from "." -- \u{02D9} is generated for over dots by most generators
				"\u{0308}" => "¨",
				_ => mo_text,
			}
			// FIX: MathType generates the wrong version of union and intersection ops (binary instead of unary)
		} else if !is_base && (parent_name == "msup" || parent_name == "msubsup") {
			mo_text = match mo_text {
				"\u{00BA}"| "\u{2092}"| "\u{20D8}"| "\u{2218}" => "\u{00B0}",		// circle-like objects -> degree
				_ => mo_text,
			};
		} else {
			mo_text = match mo_text {
				"_"| "\u{02C9}"| "\u{0304}"| "\u{0305}" => "\u{00AF}",
				"\u{01C1}" => "\u{2016}", // U+2016 is "‖"
				_ => mo_text,
			};
		};
		mo_text = match mo_text {
			"\u{2212}" => "-",
			// FIX: this needs to be after all expr the "|" has been fully canonicalized. At this point, any parent mrow/siblings is in flux
			// "\u{007C}" => {  // vertical line -> divides
			// 	// if a number or variable (lower case single letter) precedes and follows "|", switch to divides (a bit questionable...)
			// 	debug!("canonicalize_mo_text parent:\n{}", mml_to_string(&parent));
			// 	let precedes = mo.preceding_siblings();
			// 	let follows = mo.following_siblings();
			// 	if precedes.is_empty() || follows.is_empty() {
			// 		"\u{007C}"
			// 	} else {
			// 		let before = as_element(precedes[0]);
			// 		let after = as_element(follows[0]);
			// 		let before_ok = name(&before) == "mn" ||
			// 				(name(&before) == "mi" && IS_LIKELY_SCALAR_VARIABLE.is_match(as_text(before)));
			// 		let after_ok = name(&after) == "mn" ||
			// 				(name(&after) == "mi" && IS_LIKELY_SCALAR_VARIABLE.is_match(as_text(after)));
			// 		if before_ok && after_ok {"\u{2224}"} else {"\u{007C}"}
			// 	}
			// },
			_ => mo_text,
		};
		mo.set_text(mo_text);
	}
	
		
	// Find the operator associated with the 'mo_node'
	// This is complicated by potentially needing to distinguish between the
	//   prefix, infix, or postfix version of the operator.
	// To figure out prefix, we need to look at the node on the left; for postfix, we need to look to the left
	// If the node of the left has been parsed, then this works.
	// For example, suppose we want to determine if the "+" in 'x < n!+1' is prefix or infix.
	//   If we simply looked left without parsing, we'd see an operator and choose prefix unless we could figure out that
	//   that "!" was postfix.  But if it had been parsed, we'd see an mrow (operand) and tree "+" as infix (as it should).
	// The same problem applies on the right for postfix operators, but a problem is rare for those
	//   e.g., n!!n -- ((n!)!)*n or (n!)*(!n)  -- the latter doesn't make semantic sense though
	// FIX:  the above ignores mspace and other nodes that need to be skipped to determine the right node to determine airity
	// FIX:  the postfix problem above should be addressed
	fn find_operator<'a>(&self, mo_node: Element<'a>, previous_operator: Option<&'static OperatorInfo>,
						previous_node: Option<Element<'a>>, next_node: Option<Element<'a>>) -> &'static OperatorInfo {
		// get the unicode value and return the OpKeyword associated with it
		assert!( name(&mo_node) == "mo");
	
		// if a form has been given, that takes precedence
		let form = mo_node.attribute_value("form");
		let op_type =  match form {
			None => compute_type_from_position(self, previous_operator, previous_node, next_node),
			Some(form) => match form.to_lowercase().as_str() {
				"prefix" => OperatorTypes::PREFIX,
				"postfix" => OperatorTypes::POSTFIX,
				_ => OperatorTypes::INFIX,
			}
		};	
	
		let found_op_info = if mo_node.attribute_value(CHEMICAL_BOND).is_some() {
			Some(&*IMPLIED_CHEMICAL_BOND)
		} else {
			OPERATORS.get(as_text(mo_node))
		};
		if found_op_info.is_none() {
			// no known operator -- return the unknown operator with the correct "fix" type
			return op_not_in_operator_dictionary(op_type);
		}
	
		let found_op_info = found_op_info.unwrap();
		let matching_op_info = find_operator_info(found_op_info, op_type, form.is_some());
		if ptr_eq(matching_op_info, *ILLEGAL_OPERATOR_INFO) {
			return op_not_in_operator_dictionary(op_type);
		} else {
			return matching_op_info;
		}

	
		fn compute_type_from_position<'a>(context: &CanonicalizeContext, previous_operator: Option<&'static OperatorInfo>, previous_node: Option<Element<'a>>, next_node: Option<Element<'a>>) -> OperatorTypes {
			// based on choices, pick one that fits the context
			// if there isn't an obvious one, we have parsed the left, but not the right, so discount that
		
			// Trig functions have some special syntax
			// We need to to treat '-' as prefix for things like "sin -2x"
			// Need to be careful because (sin - cos)(x) needs an infix '-'
			// Return either the prefix or infix version of the operator
			if next_node.is_some() &&
			   context.is_function_name(get_possible_embellished_node(next_node.unwrap()), None) == FunctionNameCertainty::True {
				return OperatorTypes::INFIX;
			}
			if previous_node.is_some() &&
			   context.is_function_name(get_possible_embellished_node(previous_node.unwrap()), None) == FunctionNameCertainty::True {
				return OperatorTypes::PREFIX;
			}
		
			// after that special case, start with the obvious cases...
			let operand_on_left = previous_operator.is_none() || previous_operator.unwrap().is_postfix();	// operand or postfix operator
			let operand_on_right = next_node.is_some() && name(&get_possible_embellished_node(next_node.unwrap())) !="mo";			// FIX:  could improve by checking if it is a prefix op
		
			if operand_on_left && operand_on_right {
				return OperatorTypes::INFIX;	// infix
			} else if !operand_on_left && operand_on_right {
				return OperatorTypes::PREFIX;	// prefix
			} else if operand_on_left && !operand_on_right {
				return OperatorTypes::POSTFIX;	// postfix
			} else {
				// either two operators in a row or right hand side not parsed so we don't really know what is right (same is true above)
				// since there is nothing good to return, assume right is an operand after parsing (thus infix case)
				return OperatorTypes::INFIX;
			}
		}

		fn find_operator_info(op_info: &OperatorInfo, op_type: OperatorTypes, from_form_attr: bool) -> &OperatorInfo {
			if op_info.is_operator_type(op_type) {
				return op_info;
			} else if let Some(next_op_info) = op_info.next {
				if next_op_info.is_operator_type(op_type) {
					return next_op_info;
				} else if let Some(last_op_info) = next_op_info.next {
					if last_op_info.is_operator_type(op_type) {
						return last_op_info;
					}
				}
			}

			// didn't find op_info that matches -- if type is not forced, then return first value (any is probably ok) 
			return if from_form_attr {&ILLEGAL_OPERATOR_INFO} else {op_info};
		}
	
		fn op_not_in_operator_dictionary(op_type: OperatorTypes) -> &'static OperatorInfo {
			return match op_type {
				OperatorTypes::PREFIX => &DEFAULT_OPERATOR_INFO_PREFIX,
				OperatorTypes::POSTFIX => &DEFAULT_OPERATOR_INFO_POSTFIX,
				_ => &DEFAULT_OPERATOR_INFO_INFIX,	// should only be infix
			};
		}
	}
	
	fn n_vertical_bars_on_right<'a>(&self, remaining_children: &[ChildOfElement], vert_bar_ch: &'a str) -> usize {
		// return the number of children that match 'vert_bar_op' not counting the first element
		let mut n = 0;
		for child_of_element in remaining_children {
			let child = as_element(*child_of_element);
			if name(&child) == "mo" {
				let operator_str = as_text(child);
				if operator_str == vert_bar_ch {
					n += 1;
				}
			}
		}
		return n;
	}
	
	
	fn determine_vertical_bar_op<'a>(&self, original_op: &'static OperatorInfo, mo_node: Element<'a>, 
				next_child: Option<Element<'a>>,
				parse_stack: &'a mut Vec<StackInfo>,
				n_vertical_bars_on_right: usize) -> &'static OperatorInfo {
		// if in a prefix location, it is a left fence
		// note:  if there is an operator on the top of the stack, it wants an operand (otherwise it would have been reduced)
		let operator_str = as_text(mo_node);
		let found_op_info = OPERATORS.get(operator_str);
		if found_op_info.is_none() {
			return original_op;
		}
		let op = found_op_info.unwrap();
		if !AMBIGUOUS_OPERATORS.contains(operator_str) {
			// debug!("   op is not ambiguous");
			return original_op;
		};
	
		let operator_versions = OperatorVersions::new(op);
		if operator_versions.prefix.is_some() &&
		   (top(parse_stack).last_child_in_mrow().is_none() || !top(parse_stack).is_operand) {
			// debug!("   is prefix");
			return operator_versions.prefix.unwrap();
		}
		
		// We have either a right fence or an infix operand at the top of the stack
		// If this is already parsed, we'd look to the right to see if there is an operand after this child.
		// But it isn't parsed and there might be a prefix operator which will eventually become an operand, so it is tricky.
		// It is even trickier because we might have an implicit times, so we can't really tell
		// For example:  |x|y|z| which can be '|x| y |z|' or '|x |y| z|', or even | (x|y)|z |'
		// We can't really know what is intended (without @intent).
		// It seems like the case where it could be paired with a matching vertical bar as what most people would choose, so we favor that.
	
		// If there is a matching open vertical bar, it is either at the top of the stack or the entry just below the top

		let has_left_match = if let Some(op_prefix) = operator_versions.prefix {
			if ptr_eq(top(parse_stack).op_pair.op, op_prefix) { 	// match at top of stack? (empty matching bars)
				true
			} else if parse_stack.len() > 2 {
				// matching op is below top (operand between matching bars) -- pop, peek, push
				let old_top = parse_stack.pop().unwrap();		
				let top_op = top(parse_stack).op_pair.op;																	// can only access top, so we need to pop off top and push back later
				parse_stack.push(old_top);
				ptr_eq(top_op, op_prefix)
			} else {
				false
			}
		} else {
			false
		};
		if operator_versions.postfix.is_some() && (next_child.is_none() || has_left_match) {
			// last child in row (must be a close) or we have a left match
			// debug!("   is postfix");
			return operator_versions.postfix.unwrap();
		} else if next_child.is_none() {
			// operand on left, so prefer infix version
			return if operator_versions.infix.is_none() {op} else {operator_versions.infix.unwrap()};
		}
	
		let next_child = next_child.unwrap();
		if operator_versions.prefix.is_some() && (n_vertical_bars_on_right & 0x1 != 0) {
			// 	("   is prefix");
			return operator_versions.prefix.unwrap();		// odd number of vertical bars remain, so consider this the start of a pair
		}
	
		let next_child = get_possible_embellished_node(next_child);
		let next_child_op = if name(&next_child) != "mo" {
				None
			} else {
				let next_next_children = next_child.following_siblings();
				let next_next_child = if next_next_children.is_empty() { None } else { Some( as_element(next_next_children[0]) )};
				Some( self.find_operator(next_child, operator_versions.infix,
									top(parse_stack).last_child_in_mrow(), next_next_child) )
			};
												  
		// If the next child is a prefix op or a left fence, it will reduce to an operand, so don't consider it an operator
		if next_child_op.is_some() && !next_child_op.unwrap().is_left_fence() && !next_child_op.unwrap().is_prefix() {
			if operator_versions.postfix.is_some() {
				// debug!("   is postfix");
				return operator_versions.postfix.unwrap();	
			}
		} else if operator_versions.infix.is_some() {
			// debug!("   is infix");
			return operator_versions.infix.unwrap();	
		}
	
		// nothing good to match
		return op;
	}


	// return FunctionNameCertainty::False or Maybe if 'node' is a chemical element and is followed by a state (solid, liquid, ...)
	//  in other words, we are certain this can't be a function since it looks like it is or might be chemistry
	fn is_likely_chemical_state<'a>(&self, node: Element<'a>, right_sibling: Element<'a>) -> FunctionNameCertainty {
		assert_eq!(name(&node.parent().unwrap().element().unwrap()), "mrow"); // should be here because we are parsing an mrow
	
		// debug!("   in is_likely_chemical_state: '{}'?",element_summary(node));
		let node_chem_likelihood= node.attribute_value(MAYBE_CHEMISTRY);
		if node.attribute(MAYBE_CHEMISTRY).is_none() {
			return FunctionNameCertainty::True;
		}

		if name(&right_sibling) == "mrow" {		// clean_chemistry_mrow made sure any state-like structure is an mrow
			let state_likelihood = likely_chem_state(right_sibling);
			if state_likelihood > 0 {
				right_sibling.set_attribute_value(MAYBE_CHEMISTRY, state_likelihood.to_string().as_str());
				// at this point, we know both node and right_sibling are positive, so we have at least a maybe
				if state_likelihood + node_chem_likelihood.unwrap().parse::<isize>().unwrap() > 2 {
					return FunctionNameCertainty::False;
				} else {
					return FunctionNameCertainty::Maybe
				}
			}
		}

		return FunctionNameCertainty::True;
	}
	
	// Try to figure out whether an <mi> is a function name or note.
	// There are two important cases depending upon whether parens/brackets are used or not.
	// E.g, sin x and f(x)
	// 1. If parens follow the name, then we use a more inclusive set of heuristics as it is more likely a function
	// The heuristics used are:
	//   - it is on the list of known function names (e.g., sin" and "log")
	//   - it is on the list of likely function names (e.g, f, g, h)
	//   - multi-char names that begin with a capital letter (e.g, "Tr")
	//   - there is a single token inside the parens (why else would someone use parens), any name (e.g, a(x))
	//	 - if there are multiple comma-separated args
	//
	// 2. If there are no parens, then only names on the known function list are used (e.g., "sin x")
	//
	// If the name if followed by parens but doesn't fit into the above categories, we return a "maybe"
	fn is_function_name<'a>(&self, node: Element<'a>, right_siblings: Option<&[ChildOfElement<'a>]>) -> FunctionNameCertainty {
		let base_of_name = get_possible_embellished_node(node);
	
		// actually only 'mi' should be legal here, but some systems used 'mtext' for multi-char variables
		// FIX: need to allow for composition of function names. E.g, (f+g)(x) and (f^2/g)'(x)
		let node_name = name(&base_of_name);
		if node_name != "mi" && node_name != "mtext" {
			return FunctionNameCertainty::False;
		}
		// whitespace is sometimes added to the mi since braille needs it, so do a trim here to get function name
		let base_name = as_text(base_of_name).trim();
		if base_name.is_empty() {
			return FunctionNameCertainty::False;
		}
		// debug!("    is_function_name({}), {} following nodes", base_name, if right_siblings.is_none() {"No".to_string()} else {right_siblings.unwrap().len().to_string()});
		return crate::definitions::DEFINITIONS.with(|defs| {
			// names that are always function names (e.g, "sin" and "log")
			let defs = defs.borrow();
			let names = defs.get_hashset("FunctionNames").unwrap();
			// UEB seems to think "Sin" (etc) is used for "sin", so we move to lower case
			if names.contains(&base_name.to_ascii_lowercase()) {
				// debug!("     ...is in FunctionNames");
				return FunctionNameCertainty::True;	// always treated as function names
			}

			// We include shapes as function names so that △ABC makes sense since △ and
			//   the other shapes are not in the operator dictionary
			let shapes = defs.get_hashset("GeometryShapes").unwrap();
			if shapes.contains(base_name) {
				return FunctionNameCertainty::True;	// always treated as function names
			}
	
			if right_siblings.is_none() {
				return FunctionNameCertainty::False;	// only accept known names, which is tested above
			}

			// make sure that what follows starts and ends with parens/brackets
			assert_eq!(name(&node.parent().unwrap().element().unwrap()), "mrow");
			let right_siblings = right_siblings.unwrap();
			if right_siblings.is_empty() {
				// debug!("     ...right siblings not None, but zero of them");
				return FunctionNameCertainty::False;
			}

			let first_child = as_element(right_siblings[0]);
					
			// clean_chemistry wrapped up a state in an mrow and this is assumed by is_likely_chemical_state()
			let chem_state_certainty = self.is_likely_chemical_state(node, first_child);
			if chem_state_certainty != FunctionNameCertainty::True {
				// debug!("      ...is_likely_chemical_state says it is a function ={:?}", chem_state_certainty);
				return chem_state_certainty;
			}

			if name(&first_child) == "mrow" && is_left_paren(as_element(first_child.children()[0])) {
				// debug!("     ...trying again after expanding mrow");
				return self.is_function_name(node, Some(&first_child.children()));
			}

			if right_siblings.len() < 2 {
				// debug!("     ...not enough right siblings");
				return FunctionNameCertainty::False;	// can't be (...)
			}

			// at least two siblings are this point -- check that they are parens/brackets
			// we can only check the open paren/bracket because the right side is unparsed and we don't know the close location
			let first_sibling = as_element(right_siblings[0]);
			if name(&first_sibling) != "mo"  || !is_left_paren(first_sibling)  // '(' or '['
			{
				// debug!("     ...first sibling is not '(' or '['");
				return FunctionNameCertainty::False;
			}
	
			let likely_names = defs.get_hashset("LikelyFunctionNames").unwrap();
			if likely_names.contains(base_name) {
				return FunctionNameCertainty::True;	// don't bother checking contents of parens, consider these as function names
			}
	
			if is_single_arg(as_text(first_sibling), &right_siblings[1..]) {
				// debug!("      ...is single arg");
				return FunctionNameCertainty::True;	// if there is only a single arg, why else would you use parens?
			};

			if is_comma_arg(as_text(first_sibling), &right_siblings[1..]) {
				// debug!("      ...is comma arg");
				return FunctionNameCertainty::True;	// if there is only a single arg, why else would you use parens?
			};
	
			// FIX: should really make sure all the args are marked as MAYBE_CHEMISTRY, but we don't know the matching close paren/bracket
			if node.attribute(MAYBE_CHEMISTRY).is_some() &&
			   as_element(right_siblings[1]).attribute(MAYBE_CHEMISTRY).is_some() {
				return FunctionNameCertainty::False;
			}
	
			// Names like "Tr" are likely function names, single letter names like "M" or "J" are iffy
			// This needs to be after the chemical state check above to rule out Cl(g), etc
			// This would be better if if were part of 'likely_names' as "[A-Za-z]+", but reg exprs don't work in HashSets.
			// FIX: create our own struct and write appropriate traits for it and then it could work
			let mut chars = base_name.chars();
			let first_char = chars.next().unwrap();		// we know there is at least one byte in it, hence one char
			if chars.next().is_some() && first_char.is_uppercase() {
				// debug!("      ...is uppercase name");
				return FunctionNameCertainty::True;
			}

			// debug!("      ...didn't match options to be a function");
			return FunctionNameCertainty::Maybe;		// didn't fit one of the above categories
		});
	
		fn is_single_arg<'a>(open: &str, following_nodes: &[ChildOfElement<'a>]) -> bool {
			// following_nodes are nodes after "("
			if following_nodes.is_empty() {
				return true;		// "a(" might or might not be a function call -- treat as "is" because we can't see more 
			}
	
			let first_child = as_element(following_nodes[0]);
			if is_matching_right_paren(open, first_child) {
				return true;		// no-arg case "a()"
			}
	
			// could be really picky and restrict to checking for only mi/mn
			// that might make more sense in stranger cases, but mfrac, msqrt, etc., probably shouldn't have parens if times 
			return following_nodes.len() > 1 && 
					name(&first_child) != "mrow" &&
					is_matching_right_paren(open, as_element(following_nodes[1]));
		}
	
		fn is_comma_arg<'a>(open: &str, following_nodes: &[ChildOfElement<'a>]) -> bool {
			// following_nodes are nodes after "("
			if following_nodes.len() == 1 {
				return false; 
			}

			let first_child = as_element(following_nodes[1]);
			if name(&first_child) == "mrow" {
				return is_comma_arg(open, &first_child.children()[..]);
			}

			// FIX: this loop is very simplistic and could be improved to count parens, etc., to make sure "," is at top-level
			for child in following_nodes {
				let child = as_element(*child);
				if name(&child) == "mo" {
					if as_text(child) == "," {
						return true;
					}
					if is_matching_right_paren(open, child) {
						return false;
					}
				}
			}
			
			return false;
		}
	
		fn is_left_paren(node: Element) -> bool {
			if name(&node) != "mo" {
				return false;
			}
			let text = as_text(node);
			return text == "(" || text == "[";
		}
	
		fn is_matching_right_paren(open: &str, node: Element) -> bool {
			if name(&node) != "mo" {
				return false;
			}
			let text = as_text(node);
			// debug!("         is_matching_right_paren: open={}, close={}", open, text);
			return (open == "(" && text == ")") || (open == "[" && text == "]");
		}
	}
	
	fn is_mixed_fraction<'a>(&self, integer_part: &'a Element<'a>, fraction_children: &[ChildOfElement<'a>]) -> Result<bool> {
		// do some simple disqualifying checks on the fraction part
		if fraction_children.is_empty() {
			return Ok( false );
		}
		let right_child = as_element(fraction_children[0]);
		let right_child_name = name(&right_child);
		if ! (right_child_name == "mfrac" ||
			 (right_child_name == "mrow" && right_child.children().len() == 3) ||
		     (right_child_name == "mn" && fraction_children.len() >= 3) ) {
			return Ok( false );
		};

		if !is_integer_part_ok(integer_part) {
			return Ok( false );
		}
		
		if right_child_name == "mfrac" {
			return Ok( is_mfrac_ok(&right_child) );
		}

		return is_linear_fraction(self, fraction_children);


		fn is_int<'a>(integer_part: &'a Element<'a>) -> bool {
			return name(integer_part) == "mn"  && !as_text(*integer_part).contains(DECIMAL_SEPARATOR);
		}

		fn is_integer_part_ok<'a>(integer_part: &'a Element<'a>) -> bool {
			// integer part must be either 'n' or '-n' (in an mrow)
			let integer_part_name = name(integer_part);
			if integer_part_name == "mrow" {
				let children = integer_part.children();
				if children.len() == 2 &&
				   name(&as_element(children[0])) == "mo" &&
				   as_text(as_element(children[0])) == "-" {
					let integer_part = as_element(children[1]);
					return is_int(&integer_part);
				}
				return false;
			};
		
			return is_int(integer_part);
		}

		fn is_mfrac_ok<'a>(fraction_part: &'a Element<'a>) -> bool {
			// fraction_part needs to have integer numerator and denominator (already tested it is a frac)
			let fraction_children = fraction_part.children();
			if fraction_children.len() != 2 {
				return false;
			}
			let numerator = as_element(fraction_children[0]);
			if name(&numerator) != "mn" || as_text(numerator).contains(DECIMAL_SEPARATOR) {
				return false;
			}
			let denominator = as_element(fraction_children[1]);
			return is_int(&denominator);
		}

		fn is_linear_fraction<'a>(canonicalize: &CanonicalizeContext, fraction_children: &[ChildOfElement<'a>]) -> Result<bool> {
			// two possibilities
			// 1. '3 / 4' is in an mrow
			// 2. '3 / 4' are three separate elements
			let first_child = as_element(fraction_children[0]);
			if name(&first_child) == "mrow" {
				if first_child.children().len() != 3 {
					return Ok( false );
				}
				return is_linear_fraction(canonicalize, &first_child.children())
			}
			
			
			// the length has been checked
			assert!(fraction_children.len() >= 3);
			
			if !is_int(&first_child) {
				return Ok( false );
			}
			let slash_part = canonicalize.canonicalize_mrows(as_element(fraction_children[1]))?;
			if name(&slash_part) == "mo" && as_text(slash_part) == "/" {
				let denom = canonicalize.canonicalize_mrows(as_element(fraction_children[2]))?;
				return Ok( is_int(&denom) );
			}
			return Ok( false );
		}
	}

	// implied comma when two numbers are adjacent and are in a script position
	fn is_implied_comma<'a>(&self, prev: &'a Element<'a>, current: &'a Element<'a>, mrow: &'a Element<'a>) -> bool {
		if name(prev) != "mn" || name(current) != "mn" {
			return false;
		}

		assert_eq!(name(&mrow), "mrow");
		let container = mrow.parent().unwrap().element().unwrap();
		let name = name(&container);

		// test for script position is that it is not the base and hence has a preceding sibling
		return (name == "msub" || name == "msubsup" || name == "msup") && !mrow.preceding_siblings().is_empty();
	}

	// implied separator when two capital letters are adjacent or two chemical elements
	fn is_implied_chemical_bond<'a>(&self, prev: &'a Element<'a>, current: &'a Element<'a>) -> bool {
		// debug!("is_implied_chemical_bond: previous: {:?}", prev.preceding_siblings());
		// debug!("is_implied_chemical_bond: following: {:?}", prev.following_siblings());
		if prev.attribute(MAYBE_CHEMISTRY).is_none() || current.attribute(MAYBE_CHEMISTRY).is_none() {
			return false;
		}
		// ABC example where B and C are chemical elements is why we need to scan further than just checking B and C
		// look for an mi/mtext with @MAYBE_CHEMISTRY until we get to something that can't have it
		for child in prev.preceding_siblings() {
			if !is_valid_chemistry(as_element(child)) {
				return false;
			}
		}
		for child in current.following_siblings() {
			if !is_valid_chemistry(as_element(child)) {
				return false;
			}
		}
		return true;		// sequence of all MAYBE_CHEMISTRY

		fn is_valid_chemistry(child: Element) -> bool {
			let child = get_possible_embellished_node(child);
			return child.attribute(MAYBE_CHEMISTRY).is_some() || (name(&child) != "mi" && name(&child) != "mtext");
		}
	}

	// implied separator when two capital letters are adjacent or two chemical elements
	fn is_implied_separator<'a>(&self, prev: &'a Element<'a>, current: &'a Element<'a>) -> bool {
		if name(prev) != "mi" || name(current) != "mi" {
			return false;
		}

		let prev_text = as_text(*prev);
		let current_text = as_text(*current);
		return prev_text.len() == 1 && current_text.len() == 1 &&
			   is_cap(prev_text) && is_cap(current_text);


		fn is_cap(str: &str) -> bool {
			assert_eq!(str.len(), 1);
			return str.chars().next().unwrap().is_ascii_uppercase();
		}
	}
	
	// Add the current operator if it's not n-ary to the stack
	// 'current_child' and it the operator to the stack.
	fn shift_stack<'s, 'a:'s, 'op:'a>(
				&self, parse_stack: &'s mut Vec<StackInfo<'a, 'op>>,
				current_child: Element<'a>, 
				current_op: OperatorPair<'op>) -> (Element<'a>, OperatorPair<'op>) {
		let mut new_current_child = current_child;
		let mut new_current_op = current_op.clone();
		let previous_op = top(parse_stack).op_pair.clone();
		// debug!(" shift_stack: mrow len={}", top(parse_stack).mrow.children().len().to_string());
		// debug!(" shift_stack: shift on '{}'; ops: prev '{}/{}', cur '{}/{}'",
		// 		element_summary(current_child),show_invisible_op_char(previous_op.ch), previous_op.op.priority,
		// 		show_invisible_op_char(current_op.ch), current_op.op.priority);
		if !previous_op.op.is_nary(current_op.op) {
			// grab operand on top of stack (if there is one) and make it part of the new mrow since current op has higher precedence
			// if operators are the same and are binary, then this push makes them act as left associative
			let mut top_of_stack = parse_stack.pop().unwrap();
			if top_of_stack.mrow.children().is_empty() || (!top_of_stack.is_operand && !current_op.op.is_right_fence()) {
				// "bad" syntax - no operand on left -- don't grab operand (there is none)
				//   just start a new mrow beginning with operator
				// FIX -- check this shouldn't happen:  parse_stack.push(top_of_stack);
				parse_stack.push( top_of_stack );		// put top back on
				parse_stack.push( StackInfo::new(current_child.document()) );
			} else if current_op.op.is_right_fence() {
				// likely, but not necessarily, there is a left fence to start the mrow
				// this is like the postfix case except we grab the entire mrow, push on the close, and make that the mrow
				// note:  the code does these operations on the stack for consistency, but it could be optimized without push/popping the stack
				let mrow = top_of_stack.mrow;
				top_of_stack.add_child_to_mrow(current_child, current_op);
				// debug!("shift_stack: after adding right fence to mrow: {}", mml_to_string(&top_of_stack.mrow));
				new_current_op = OperatorPair::new();							// treat matched brackets as operand
				new_current_child = mrow;	
				let children = mrow.children();
				if  children.len() == 2 &&
					( name(&as_element(children[0])) != "mo" ||
					  !self.find_operator(as_element(children[0]),
								   None, Some(as_element(children[0])), Some(mrow) ).is_left_fence()) {
					// the mrow did *not* start with an open (hence no push)
					// since parser really wants balanced parens to keep stack state right, we do a push here
					parse_stack.push( StackInfo::new(mrow.document()) );
				} else if children.len() <= 3 {
					// the mrow started with some open fence (which caused a push) -- add the close, pop, and push on the "operand"
					new_current_child = self.potentially_lift_script(mrow)
				} else {
					panic!("Wrong number of children in mrow when handling a close fence");
				}
			} else if current_op.op.is_postfix() {
				// grab the left operand and start a new mrow with it and the operator -- put those back on the stack
				// note:  the code does these operations on the stack for consistency, but it could be optimized without push/popping the stack
				let previous_child = top_of_stack.remove_last_operand_from_mrow();					// remove operand from mrow
				parse_stack.push(top_of_stack);
				let mut new_top_of_stack = StackInfo::with_op(&current_child.document(), previous_child, current_op.clone()); // begin new mrow with operand
				new_top_of_stack.add_child_to_mrow(current_child, current_op);	// add on operator
				new_current_child = new_top_of_stack.mrow;								// grab for pushing on old mrow
				new_current_op = OperatorPair::new();								// treat "reduced" postfix operator & operand as an operand
				// debug!("shift_stack: after adding postfix to mrow has len: {}", new_current_child.children().len().to_string());
			} else {
				// normal infix op case -- grab the left operand and start a new mrow with it and the operator
				let previous_child = top_of_stack.remove_last_operand_from_mrow();
				parse_stack.push(top_of_stack);
				parse_stack.push( StackInfo::with_op(&current_child.document(),previous_child, current_op) );
			}
		}
		return (new_current_child, new_current_op);
	}
	
	
	fn reduce_stack<'s, 'a:'s, 'op:'a>(&self, parse_stack: &'s mut Vec<StackInfo<'a, 'op>>, current_priority: usize) {
		let mut prev_priority = top(parse_stack).priority();
		// debug!(" reduce_stack: stack len={}, priority: prev={}, cur={}", parse_stack.len(), prev_priority, current_priority);
		while current_priority < prev_priority {					// pop off operators until we are back to the right level
			if parse_stack.len() == 1 {
				break;			// something went wrong -- break before popping too much
			}
			prev_priority = self.reduce_stack_one_time(parse_stack);
		};
	}

	fn reduce_stack_one_time<'s, 'a:'s, 'op:'a>(&self, parse_stack: &'s mut Vec<StackInfo<'a, 'op>>) -> usize {
		let mut top_of_stack = parse_stack.pop().unwrap();
		// debug!(" ..popped len={} op:'{}/{}', operand: {}",
		// 		top_of_stack.mrow.children().len(),
		// 		show_invisible_op_char(top_of_stack.op_pair.ch), top_of_stack.op_pair.op.priority,
		// 		top_of_stack.is_operand);
		let mut mrow = top_of_stack.mrow;
		if mrow.children().len() == 1 {
			// should have added at least operator and operand, but input might not be well-formed
			// in this case, unwrap the mrow and expose the single child for pushing onto stack
			let single_child = top_of_stack.remove_last_operand_from_mrow();
			mrow = single_child;
		}

		let mut top_of_stack = parse_stack.pop().unwrap();
		top_of_stack.add_child_to_mrow(mrow, OperatorPair::new());	// mrow on top is "parsed" -- now add it to previous
		let prev_priority = top_of_stack.priority();
		parse_stack.push(top_of_stack);
		return prev_priority;
	}
	
	fn is_trig_arg<'a, 'op:'a>(&self, previous_child: Element<'a>, current_child: Element<'a>, parse_stack: &mut Vec<StackInfo<'a, 'op>>) -> bool {
		// We have operand-operand and know we want multiplication at this point. 
		// Check for special case where we want multiplication to bind more tightly than function app (e.g, sin 2x, sin -2xy)
		// We only want to do this for simple args
		use crate::xpath_functions::IsNode;
		// debug!("  is_trig_arg: prev {}, current {}, Stack:", element_summary(previous_child), element_summary(current_child));
		// parse_stack.iter().for_each(|stack_info| debug!("    {}", stack_info));
		if !IsNode::is_simple(&current_child) {
			return false;
		}
		// This only matters if we are not inside of parens
		if IsBracketed::is_bracketed(&previous_child, "(", ")", false, false) ||
		   IsBracketed::is_bracketed(&previous_child, "[", "]", false, false) {
			return false;
		}
	
		// Use lower priority multiplication if current_child is a function (e.g. "cos" in "sin x cos 3y")
		// if !is_trig(current_child) {
		if self.is_function_name(current_child, None) == FunctionNameCertainty::True {
			return false;
		}
		// Three cases:
		// 1. First operand-operand (e.g, sin 2x, where 'current_child' is 'x') -- top of stack is mrow('sin' f_apply '2')
		// 2. Another First operand-operand (e.g, sin -2x, where 'current_child' is 'x') -- top of stack is mrow('-' '2'), next is mrow('sin', f_apply)
		// 3. Subsequent operand-operand (e.g, sin 2xy, where 'current_child' is 'y') -- top of stack is mrow('2' 'times' 'x')
		//    Note: IMPLIED_TIMES_HIGH_PRIORITY is only present if we have a trig function
		let op_on_top = &top(parse_stack).op_pair;
		if ptr_eq(op_on_top.op, *INVISIBLE_FUNCTION_APPLICATION) {
			let function_element = as_element(top(parse_stack).mrow.children()[0]);
			return is_trig(function_element);
		}
		if ptr_eq(op_on_top.op, *PREFIX_MINUS) {
			if parse_stack.len() < 2 {
				return false;
			}
			let next_stack_info = &parse_stack[parse_stack.len()-2];
			if !ptr_eq(next_stack_info.op_pair.op, *INVISIBLE_FUNCTION_APPLICATION) {
				return false;
			}
			let function_element = as_element(next_stack_info.mrow.children()[0]);
			if is_trig(function_element) {
				// want '- 2' to be an mrow; don't want '- 2 x ...' to be the mrow (IMPLIED_TIMES_HIGH_PRIORITY is an internal hack)
				self.reduce_stack_one_time(parse_stack);
				return true;
			}
			return false;
		}
		return ptr_eq(op_on_top.op, &*IMPLIED_TIMES_HIGH_PRIORITY);

		fn is_trig(node: Element) -> bool {
			let base_of_name = get_possible_embellished_node(node);
	
			// actually only 'mi' should be legal here, but some systems used 'mtext' for multi-char variables
			let node_name = name(&base_of_name);
			if node_name != "mi" && node_name != "mtext" {
				return false;
			}
			// whitespace is sometimes added to the mi since braille needs it, so do a trim here to get function name
			let base_name = as_text(base_of_name).trim();
			if base_name.is_empty() {
				return false;
			}
			return crate::definitions::DEFINITIONS.with(|defs| {
				// names that are always function names (e.g, "sin" and "log")
				let defs = defs.borrow();
				let names = defs.get_hashset("TrigFunctionNames").unwrap();
				// UEB seems to think "Sin" (etc) is used for "sin", so we move to lower case
				return names.contains(&base_name.to_ascii_lowercase());
			});
		}
	}
	
	
	/*
		canonicalize_mrows_in_mrow is a simple(ish) operator precedence parser.
		It works by keeping a stack of 'StackInfo':
		'StackInfo' has three parts:
		1. the mrow being build
		2. info about the operator in the mrow being build
		3. bool to say whether the last thing is an operator or an operand
	
		When the op priority increases (eg, have "=" and get "+"), we push on
		1. a new mrow -- if the operator has a left operand, we remove the last node in the mrow and it becomes
		   the first (only so far) child of the new mrow
		2. the operator info
	
		When the op priority decreases, we do the following loop until the this new priority > priority on top of stack
		1. pop the StackInfo
		2. add the StackInfo's mrow  as the last child to the new top of the stack
		We also do this when we hit the end of the mrow (we can treat this case as if we have a negative precedence)
	
		+/- are treated as nary operators and don't push/pop in those cases.
		consecutive operands such as nary times are also considered n-ary operators and don't push/pop in those cases.
	*/
	fn canonicalize_mrows_in_mrow<'a>(&self, mrow: Element<'a>) -> Result<Element<'a>> {
		let saved_mrow_attrs = mrow.attributes();	
		assert_eq!(name(&mrow), "mrow");
	
		// FIX: don't touch/canonicalize
		// 1. if intent is given -- anything intent references
		// 2. if the mrow starts or ends with a fence, don't merge into parent (parse children only) -- allows for "]a,b["
		let mut parse_stack = vec![StackInfo::new(mrow.document())];
		let mut children = mrow.children();
		let num_children = children.len();
	
		for i_child in 0..num_children {
			// debug!("\nDealing with child #{}: {}", i_child, mml_to_string(&as_element(children[i_child])));
			let mut current_child = self.canonicalize_mrows(as_element(children[i_child]))?;
			children[i_child] = ChildOfElement::Element( current_child );
			let base_of_child = get_possible_embellished_node(current_child);

			let mut current_op = OperatorPair::new();
			// figure what the current operator is -- it either comes from the 'mo' (if we have an 'mo') or it is implied
			if name(&base_of_child) == "mo" &&
			   !( base_of_child.children().is_empty() || IS_WHITESPACE.is_match(as_text(base_of_child)) ) { // shouldn't have empty mo node, but...
				let previous_op = if top(&parse_stack).is_operand {None} else {Some( top(&parse_stack).op_pair.op )};
				let next_node = if i_child + 1 < num_children {Some(as_element(children[i_child+1]))} else {None};
				current_op = OperatorPair{
					ch: as_text(base_of_child),
					op: self.find_operator(base_of_child, previous_op,
							top(&parse_stack).last_child_in_mrow(), next_node)
				};
	
				// deal with vertical bars which might be infix, open, or close fences
				// note: mrow shrinks as we iterate through it (removing children from it)
				current_op.op = self.determine_vertical_bar_op(
					current_op.op,
					base_of_child,
					next_node,
					&mut parse_stack,
					self.n_vertical_bars_on_right(&children[i_child+1..], current_op.ch)
				);
			} else if top(&parse_stack).last_child_in_mrow().is_some() {
				let previous_child = top(&parse_stack).last_child_in_mrow().unwrap();
				let base_of_previous_child = get_possible_embellished_node(previous_child);
				if name(&base_of_previous_child) != "mo" {
					// consecutive operands -- add an invisible operator as appropriate
					let likely_function_name = self.is_function_name(previous_child, Some(&children[i_child..]));
					current_op = if likely_function_name == FunctionNameCertainty::True {
								OperatorPair{ ch: "\u{2061}", op: &INVISIBLE_FUNCTION_APPLICATION }
							} else if self.is_mixed_fraction(&previous_child, &children[i_child..])? {
								OperatorPair{ ch: "\u{2064}", op: &IMPLIED_INVISIBLE_PLUS }
							} else if self.is_implied_comma(&previous_child, &current_child, &mrow) {
								OperatorPair{ch: "\u{2063}", op: &IMPLIED_INVISIBLE_COMMA }				  
							} else if self.is_implied_chemical_bond(&previous_child, &current_child) {
								OperatorPair{ch: "\u{2063}", op: &IMPLIED_CHEMICAL_BOND }				  
							} else if self.is_implied_separator(&previous_child, &current_child) {
								OperatorPair{ch: "\u{2063}", op: &IMPLIED_SEPARATOR_HIGH_PRIORITY }				  
							} else if self.is_trig_arg(base_of_previous_child, base_of_child, &mut parse_stack) {
								OperatorPair{ch: "\u{2062}", op: &IMPLIED_TIMES_HIGH_PRIORITY }				  
							} else {
								OperatorPair{ ch: "\u{2062}", op: &IMPLIED_TIMES }
							};
	
					if name(&base_of_child) == "mo" {
						current_op.ch = as_text(base_of_child);
						// debug!("  Found whitespace op '{}'/{}", show_invisible_op_char(current_op.ch), current_op.op.priority);
					} else {
						// debug!("  Found implicit op {}/{} [{:?}]", show_invisible_op_char(current_op.ch), current_op.op.priority, likely_function_name);
						self.reduce_stack(&mut parse_stack, current_op.op.priority);
		
						let implied_mo = create_mo(current_child.document(), current_op.ch, ADDED_ATTR_VALUE);
						if likely_function_name == FunctionNameCertainty::Maybe {
							implied_mo.set_attribute_value("data-function-guess", "true");
						}
						let shift_result = self.shift_stack(&mut parse_stack, implied_mo, current_op.clone());
						// ignore shift_result.0 which is just 'implied_mo'
						assert_eq!(implied_mo, shift_result.0);
						assert!( ptr_eq(current_op.op, shift_result.1.op) );
						let mut top_of_stack = parse_stack.pop().unwrap();
						top_of_stack.add_child_to_mrow(implied_mo, current_op);
						parse_stack.push(top_of_stack);
						current_op = OperatorPair::new();	
					}
				}
			}
	
			if !ptr_eq(current_op.op, *ILLEGAL_OPERATOR_INFO) {
				if current_op.op.is_left_fence() || current_op.op.is_prefix() {
					if top(&parse_stack).is_operand {
						// will end up with operand operand -- need to choose operator associated with prev child
						// we use the original input here because in this case, we need to look to the right of the ()s to deal with chemical states
						let likely_function_name = self.is_function_name(as_element(children[i_child-1]), Some(&children[i_child..]));
						let implied_operator = if likely_function_name== FunctionNameCertainty::True {
								OperatorPair{ ch: "\u{2061}", op: &INVISIBLE_FUNCTION_APPLICATION }
							} else {
								OperatorPair{ ch: "\u{2062}", op: &IMPLIED_TIMES }
							};
						// debug!("  adding implied {}", if ptr_eq(implied_operator.op,*IMPLIED_TIMES) {"times"} else {"function apply"});
	
						let implied_mo = create_mo(current_child.document(), implied_operator.ch, ADDED_ATTR_VALUE);
						if likely_function_name == FunctionNameCertainty::Maybe {
							implied_mo.set_attribute_value("data-function-guess", "true");
						}
						let shift_result = self.shift_stack(&mut parse_stack, implied_mo, implied_operator.clone());
						// ignore shift_result.0 which is just 'implied_mo'
						assert_eq!(implied_mo, shift_result.0);
						assert!( ptr_eq(implied_operator.op, shift_result.1.op) );
						let mut top_of_stack = parse_stack.pop().unwrap();
						top_of_stack.add_child_to_mrow(implied_mo, implied_operator);
						parse_stack.push(top_of_stack);
					}
					// starting a new mrow
					parse_stack.push( StackInfo::new(current_child.document()) );
				} else {
					// One of infix, postfix, or right fence -- all should have a left operand
					// pop the stack if it is lower precedence (it forms an mrow)
					
					// hack to get linear mixed fractions to parse correctly
					if current_op.ch == "/" && top(&parse_stack).op_pair.ch == "\u{2064}" {
							current_op.op = &IMPLIED_PLUS_SLASH_HIGH_PRIORITY;
					}
					self.reduce_stack(&mut parse_stack, current_op.op.priority);
					// push new operator on stack (already handled n-ary case)
					let shift_result = self.shift_stack(&mut parse_stack, current_child, current_op);
					current_child = shift_result.0;
					current_op = shift_result.1;
				}
			}
			let mut top_of_stack = parse_stack.pop().unwrap();
			top_of_stack.add_child_to_mrow(current_child, current_op);
			parse_stack.push(top_of_stack);
		}
	
		// Reached the end -- force reduction of what's left on the stack
		self.reduce_stack(&mut parse_stack, LEFT_FENCEPOST.priority);
	
		// We essentially have 'terminator( mrow terminator)'
		//   in other words, we have an extra mrow with one child due to the initial start -- remove it
		let mut top_of_stack = parse_stack.pop().unwrap();
		assert_eq!(parse_stack.len(), 0);
	
		let mut parsed_mrow = top_of_stack.mrow;
		assert_eq!( name(&top_of_stack.mrow), "mrow");
		if parsed_mrow.children().len() == 1 {
			parsed_mrow = top_of_stack.remove_last_operand_from_mrow();
			// was synthesized, but is really the original top level mrow
		}
	
		parsed_mrow.remove_attribute(CHANGED_ATTR);
		return Ok( add_attrs(parsed_mrow, saved_mrow_attrs) );
	}	
}

// ---------------- useful utility functions --------------------
fn top<'s, 'a:'s, 'op:'a>(vec: &'s[StackInfo<'a, 'op>]) -> &'s StackInfo<'a, 'op> {
	return &vec[vec.len()-1];
}
// Replace the attrs of 'mathml' with 'attrs' and keep the global attrs of 'mathml' (i.e, lift 'attrs' to 'mathml' for replacing children)
fn add_attrs<'a>(mathml: Element<'a>, attrs: Vec<Attribute>) -> Element<'a> {
	static GLOBAL_ATTRS: phf::Set<&str> = phf_set! {
		"class", "dir", "displaystyle", "id", "mathbackground", "mathcolor", "mathsize",
		"mathvariant", "nonce", "scriptlevel", "style", "tabindex",
		"intent", "arg",
	};
	
	// debug!(   "Adding back {} attr(s) to {}", attrs.len(), name(&mathml));
	// remove non-global attrs
	for attr in mathml.attributes() {
		let attr_name = attr.name().local_part();
		if !( attr_name.starts_with("data-") || GLOBAL_ATTRS.contains(attr_name) ||
		      attr_name.starts_with("on") ) {			// allows too much - cheapo way to allow event handlers like "onchange"
			mathml.remove_attribute(attr.name());
		}
	}

	// add in 'attrs'
	for attr in attrs {
		mathml.set_attribute_value(attr.name(), attr.value());
	}
	return mathml;
}


pub fn name<'a>(node: &'a Element<'a>) -> &str {
	return node.name().local_part();
}

// The child of a non-leaf element must be an element
// Note: can't use references as that results in 'returning use of local variable'
pub fn as_element(child: ChildOfElement) -> Element {
	return match child {
		ChildOfElement::Element(e) => e,
		_ => {
			panic!("as_element: internal error -- found non-element child (text? '{:?}')", child.text());
		},
	};
}

// The child of a leaf element must be text (previously trimmed)
// Note: trim() combines all the Text children into a single string
pub fn as_text(leaf_child: Element) -> &str {
	assert!(is_leaf(leaf_child) || name(&leaf_child) == crate::infer_intent::LITERAL_NAME);
	let children = leaf_child.children();
	if children.is_empty() {
		return "";
	}
	assert!(children.len() == 1);
	return match children[0] {
		ChildOfElement::Text(t) => t.text(),
		_ => panic!("as_text: internal error -- found non-text child of leaf element"),
	}
}

#[allow(dead_code)] // for debugging with println
fn element_summary(mathml: Element) -> String {
	return format!("{}<{}>", name(&mathml),
	              if is_leaf(mathml) {show_invisible_op_char(as_text(mathml)).to_string()}
				  else 
				  					 {mathml.children().len().to_string()});
}

fn create_mo<'a, 'd:'a>(doc: Document<'d>, ch: &'a str, attr_value: &str) -> Element<'d> {
	let implied_mo = create_mathml_element(&doc, "mo");
	implied_mo.set_attribute_value(CHANGED_ATTR, attr_value);
	let mo_text = doc.create_text(ch);
	implied_mo.append_child(mo_text);
	return implied_mo;
}

fn is_adorned_node<'a>(node: &'a Element<'a>) -> bool {
	let name = name(node);
	return	name == "msub" || name == "msup" || name == "msubsup" ||
			name == "munder" || name == "mover" || name == "munderover" ||
			name == "mmultiscripts";
}

/// return 'node' or if it is adorned, return its base (recursive)
pub fn get_possible_embellished_node(node: Element) -> Element {
	let mut node = node;
	while is_adorned_node(&node) {
		node = as_element(node.children()[0]);
	}
	return node;
}		

#[allow(dead_code)] // for debugging with println
fn show_invisible_op_char(ch: &str) -> &str {
	return match ch.chars().next().unwrap() {
		'\u{2061}' => "&#x2061;",
		'\u{2062}' => "&#x2062;",
		'\u{2063}' => "&#x2063;",
		'\u{2064}' => "&#x2064;",
		'\u{E000}' => "&#xE000;",
		_ 		   => ch
	};
}


#[cfg(test)]
mod canonicalize_tests {
	#[allow(unused_imports)]
	use super::super::init_logger;
	use super::super::are_strs_canonically_equal;
    use super::*;
    use sxd_document::parser;


    #[test]
    fn canonical_same() {
        let target_str = "<math><mrow><mo>-</mo><mi>a</mi></mrow></math>";
        assert!(are_strs_canonically_equal(target_str, target_str));
    }

	#[test]
    fn plane1_common() {
        let test_str = "<math>
				<mi mathvariant='normal'>sin</mi> <mo>,</mo>		<!-- shouldn't change -->
				<mi mathvariant='italic'>bB4</mi> <mo>,</mo>		<!-- shouldn't change -->
				<mi mathvariant='bold'>a</mi> <mo>,</mo>			<!-- single char id tests -->
				<mi mathvariant='bold'>Z</mi> <mo>,</mo>
				<mn mathvariant='bold'>19=&#x1D7D7;</mn> <mo>,</mo>	<!-- '=' and plane1 shouldn't change -->
				<mn mathvariant='double-struck'>024689</mn> <mo>,</mo>	<!-- '=' and plane1 shouldn't change -->
				<mi mathvariant='double-struck'>yzCHNPQRZ</mi> <mo>,</mo>
				<mi mathvariant='fraktur'>0yACHIRZ</mi> <mo>,</mo>	<!-- 0 stays as ASCII -->
				<mi mathvariant='bold-fraktur'>nC</mi> <mo>,</mo>
				<mi mathvariant='script'>ABEFHILMRegow</mi> <mo>,</mo>
				<mi mathvariant='bold-script'>fG*</mi>				<!-- '*' shouldn't change -->
			</math>";
        let target_str = "<math>
				<mrow data-changed='added'>
					<mi mathvariant='normal'>sin</mi>
					<mo >,</mo>
					<mi mathvariant='italic'>bB4</mi>
					<mo>,</mo>
					<mi mathvariant='bold'>𝐚</mi>
					<mo>,</mo>
					<mi mathvariant='bold'>𝐙</mi>
					<mo>,</mo>
					<mn mathvariant='bold'>𝟏𝟗=𝟗</mn>
					<mo>,</mo>
					<mn mathvariant='double-struck'>𝟘𝟚𝟜𝟞𝟠𝟡</mn>
					<mo>,</mo>
					<mi mathvariant='double-struck'>𝕪𝕫ℂℍℕℙℚℝℤ</mi>
					<mo>,</mo>
					<mi mathvariant='fraktur'>0𝔶𝔄ℭℌℑℜℨ</mi>
					<mo>,</mo>
					<mi mathvariant='bold-fraktur'>𝖓𝕮</mi>
					<mo>,</mo>
					<mi mathvariant='script'>𝒜ℬℰℱℋℐℒℳℛℯℊℴ𝓌</mi>
					<mo>,</mo>
					<mi mathvariant='bold-script'>𝓯𝓖*</mi>
				</mrow>
			</math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}
	
	#[test]
    fn plane1_font_styles() {
        let test_str = "<math>
				<mi mathvariant='sans-serif'>aA09=</mi> <mo>,</mo>			<!-- '=' shouldn't change -->
				<mi mathvariant='bold-sans-serif'>zZ09</mi> <mo>,</mo>	
				<mi mathvariant='sans-serif-italic'>azAZ09</mi> <mo>,</mo>	<!-- italic digits don't exist: revert to sans-serif -->
				<mi mathvariant='sans-serif-bold-italic'>AZaz09</mi> <mo>,</mo>	<!--  italic digits don't exist: revert to just bold -->
				<mi mathvariant='monospace'>aA09</mi>
			</math>";
        let target_str = "<math>
				<mrow data-changed='added'>
					<mi mathvariant='sans-serif'>𝖺𝖠𝟢𝟫=</mi>
					<mo>,</mo>
					<mi mathvariant='bold-sans-serif'>𝘇𝗭𝟬𝟵</mi>
					<mo>,</mo>
					<mi mathvariant='sans-serif-italic'>𝘢𝘻𝘈𝘡𝟢𝟫</mi>
					<mo>,</mo>
					<mi mathvariant='sans-serif-bold-italic'>𝘼𝙕𝙖𝙯𝟬𝟵</mi>
					<mo>,</mo>
					<mi mathvariant='monospace'>𝚊𝙰𝟶𝟿</mi>
				</mrow>
			</math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}
	
	#[test]
    fn plane1_greek() {
        let test_str = "<math>
				<mi mathvariant='normal'>ΑΩαω∇∂ϵ=</mi> <mo>,</mo>		<!-- shouldn't change -->
				<mi mathvariant='italic'>ϴΑΩαω∇∂ϵ</mi> <mo>,</mo>
				<mi mathvariant='bold'>ΑΩαωϝϜ</mi> <mo>,</mo>	
				<mi mathvariant='double-struck'>Σβ∇</mi> <mo>,</mo>		<!-- shouldn't change -->
				<mi mathvariant='fraktur'>ΞΦλϱ</mi> <mo>,</mo>			<!-- shouldn't change -->
				<mi mathvariant='bold-fraktur'>ψΓ</mi> <mo>,</mo>		<!-- map to bold -->
				<mi mathvariant='script'>μΨ</mi> <mo>,</mo>				<!-- shouldn't change -->
				<mi mathvariant='bold-script'>Σπ</mi>					<!-- map to bold -->
			</math>";
        let target_str = "<math>
				<mrow data-changed='added'>
					<mi mathvariant='normal'>ΑΩαω∇∂ϵ=</mi>
					<mo>,</mo>
					<mi mathvariant='italic'>𝛳𝛢𝛺𝛼𝜔𝛻𝜕𝜖</mi>
					<mo>,</mo>
					<mi mathvariant='bold'>𝚨𝛀𝛂𝛚𝟋𝟊</mi>
					<mo>,</mo>
					<mi mathvariant='double-struck'>Σβ∇</mi>
					<mo>,</mo>
					<mi mathvariant='fraktur'>ΞΦλϱ</mi>
					<mo>,</mo>
					<mi mathvariant='bold-fraktur'>𝛙𝚪</mi>
					<mo>,</mo>
					<mi mathvariant='script'>μΨ</mi>
					<mo>,</mo>
					<mi mathvariant='bold-script'>𝚺𝛑</mi>
				</mrow>
			</math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}
	
	#[test]
    fn plane1_greek_font_styles() {
        let test_str = "<math>
				<mi mathvariant='sans-serif'>ΑΩαω∇∂ϵ=</mi> <mo>,</mo>			<!-- '=' shouldn't change -->
				<mi mathvariant='bold-sans-serif'>ϴ0ΑΩαω∇∂ϵ</mi> <mo>,</mo>	
				<mi mathvariant='sans-serif-italic'>aΑΩαω∇∂ϵ</mi> <mo>,</mo>	<!-- italic digits don't exist: revert to sans-serif -->
				<mi mathvariant='sans-serif-bold-italic'>ZΑΩαωϰϕϱϖ</mi> <mo>,</mo>	<!--  italic digits don't exist: revert to just bold -->
				<mi mathvariant='monospace'>zΑΩαω∇∂</mi>
			</math>";
        let target_str = "<math>
				<mrow data-changed='added'>
					<mi mathvariant='sans-serif'>ΑΩαω∇∂ϵ=</mi>
					<mo>,</mo>
					<mi mathvariant='bold-sans-serif'>𝝧𝟬𝝖𝝮𝝰𝞈𝝯𝞉𝞊</mi>
					<mo>,</mo>
					<mi mathvariant='sans-serif-italic'>𝘢ΑΩαω∇∂ϵ</mi>
					<mo>,</mo>
					<mi mathvariant='sans-serif-bold-italic'>𝙕𝞐𝞨𝞪𝟂𝟆𝟇𝟈𝟉</mi>
					<mo>,</mo>
					<mi mathvariant='monospace'>𝚣ΑΩαω∇∂</mi>
				</mrow>
			</math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}

    #[test]
    fn short_and_long_dash() {
        let test_str = "<math><mi>x</mi> <mo>=</mo> <mi>--</mi><mo>+</mo><mtext>----</mtext></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
			<mi>x</mi>
			<mo>=</mo>
			<mrow data-changed='added'>
				<mi>—</mi>
				<mo>+</mo>
				<mtext>―</mtext>
			</mrow>
			</mrow>
		</math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn illegal_mathml_element() {
		use crate::interface::*;
        let test_str = "<math><foo><mi>f</mi></foo></math>";
        let package1 = &parser::parse(test_str).expect("Failed to parse test input");
		let mathml = get_element(package1);
		trim_element(&mathml);
		assert!(canonicalize(mathml).is_err());
    }


    #[test]
    fn mfenced_no_children() {
        let test_str = "<math><mi>f</mi><mfenced><mrow/></mfenced></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
				<mi>f</mi>
				<mo data-changed='added'>&#x2061;</mo>
				<mrow>
					<mo data-changed='from_mfenced'>(</mo>
					<mo data-changed='from_mfenced'>)</mo>
				</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn mfenced_one_child() {
        let test_str = "<math><mi>f</mi><mfenced open='[' close=']'><mi>x</mi></mfenced></math>";
        let target_str = " <math>
			<mrow data-changed='added'>
			<mi>f</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow>
				<mo data-changed='from_mfenced'>[</mo>
				<mi>x</mi>
				<mo data-changed='from_mfenced'>]</mo>
			</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn mfenced_no_attrs() {
        let test_str = "<math><mi>f</mi><mfenced><mrow><mi>x</mi><mo>,</mo><mi>y</mi><mo>,</mo><mi>z</mi></mrow></mfenced></math>";
        let target_str = " <math>
			<mrow data-changed='added'>
			<mi>f</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow>
				<mo data-changed='from_mfenced'>(</mo>
				<mrow>
				<mi>x</mi>
				<mo>,</mo>
				<mi>y</mi>
				<mo>,</mo>
				<mi>z</mi>
				</mrow>
				<mo data-changed='from_mfenced'>)</mo>
			</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn mfenced_with_separators() {
        let test_str = "<math><mi>f</mi><mfenced separators=',;'><mi>x</mi><mi>y</mi><mi>z</mi><mi>a</mi></mfenced></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
			<mi>f</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow>
				<mo data-changed='from_mfenced'>(</mo>
				<mrow data-changed='added'>
				<mrow data-changed='added'>
					<mi>x</mi>
					<mo data-changed='from_mfenced'>,</mo>
					<mi>y</mi>
				</mrow>
				<mo data-changed='from_mfenced'>;</mo>
				<mrow data-changed='added'>
					<mi>z</mi>
					<mo data-changed='from_mfenced'>,</mo>
					<mi>a</mi>
				</mrow>
				</mrow>
				<mo data-changed='from_mfenced'>)</mo>
			</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn canonical_one_element_mrow_around_mrow() {
        let test_str = "<math><mrow><mrow><mo>-</mo><mi>a</mi></mrow></mrow></math>";
        let target_str = "<math><mrow><mo>-</mo><mi>a</mi></mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn mn_with_negative_sign() {
		// init_logger();
        let test_str = "<math><mfrac>
				<mrow><mn>-1</mn></mrow>
				<mn>−987</mn>
				</mfrac></math>";
        let target_str = "<math><mfrac>
			<mrow data-changed='added'><mo>-</mo><mn>1</mn></mrow>
			<mrow data-changed='added'><mo>-</mo><mn>987</mn></mrow>
			</mfrac></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn canonical_one_element_mrow_around_mo() {
        let test_str = "<math><mrow><mrow><mo>-</mo></mrow><mi>a</mi></mrow></math>";
        let target_str = "<math><mrow><mo>-</mo><mi>a</mi></mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn canonical_flat_to_times_and_plus() {
        let test_str = "<math><mi>c</mi><mo>+</mo><mi>x</mi><mi>y</mi></math>";
        let target_str = "<math>
		<mrow data-changed='added'><mi>c</mi><mo>+</mo>
		  <mrow data-changed='added'><mi>x</mi><mo data-changed='added'>&#x2062;</mo><mi>y</mi></mrow>
		</mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn canonical_prefix_and_infix() {
        let test_str = "<math><mrow><mo>-</mo><mi>a</mi><mo>-</mo><mi>b</mi></mrow></math>";
        let target_str = "<math>
		<mrow>
		  <mrow data-changed='added'>
			<mo>-</mo>
			<mi>a</mi>
		  </mrow>
		  <mo>-</mo>
		  <mi>b</mi>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn function_with_single_arg() {
        let test_str = "<math><mrow>
			<mi>sin</mi><mo>(</mo><mi>x</mi><mo>)</mo>
			<mo>+</mo>
			<mi>f</mi><mo>(</mo><mi>x</mi><mo>)</mo>
			<mo>+</mo>
			<mi>t</mi><mrow><mo>(</mo><mi>x</mi><mo>)</mo></mrow>
		</mrow></math>";
        let target_str = "<math>
		<mrow>
		  <mrow data-changed='added'>
			<mi>sin</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow data-changed='added'>
			  <mo>(</mo>
			  <mi>x</mi>
			  <mo>)</mo>
			</mrow>
		  </mrow>
		  <mo>+</mo>
		  <mrow data-changed='added'>
			<mi>f</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow data-changed='added'>
			  <mo>(</mo>
			  <mi>x</mi>
			  <mo>)</mo>
			</mrow>
		  </mrow>
		  <mo>+</mo>
		  <mrow data-changed='added'>
			<mi>t</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow>
			  <mo>(</mo>
			  <mi>x</mi>
			  <mo>)</mo>
			</mrow>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

	#[test]
	fn maybe_function() {
		let test_str = "<math>
				<mrow>
					<mi>P</mi>
					<mo>(</mo>
					<mi>A</mi>
					<mo>∩</mo>
					<mi>B</mi>
					<mo>)</mo>
				</mrow>
			</math>";
		let target_str = "<math>
				<mrow>
				<mi>P</mi>
				<mo data-function-guess='true' data-changed='added'>&#x2062;</mo>
				<mrow data-changed='added'>
					<mo>(</mo>
					<mrow data-changed='added'>
					<mi>A</mi>
					<mo>∩</mo>
					<mi>B</mi>
					</mrow>
					<mo>)</mo>
				</mrow>
				</mrow>
			</math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}

    #[test]
    fn function_with_multiple_args() {
        let test_str = "<math>
		<mi>sin</mi><mo>(</mo><mi>x</mi><mo>+</mo><mi>y</mi><mo>)</mo>
			<mo>+</mo>
		 <mi>f</mi><mo>(</mo><mi>x</mi><mo>+</mo><mi>y</mi><mo>)</mo>
			<mo>+</mo>
		 <mi>t</mi><mo>(</mo><mi>x</mi><mo>+</mo><mi>y</mi><mo>)</mo>
			<mo>+</mo>
		 <mi>w</mi><mo>(</mo><mi>x</mi><mo>,</mo><mi>y</mi><mo>)</mo>
		</math>";
        let target_str = " <math>
		<mrow data-changed='added'>
		<mrow data-changed='added'>
		  <mi>sin</mi>
		  <mo data-changed='added'>&#x2061;</mo>
		  <mrow data-changed='added'>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>+</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
		<mo>+</mo>
		<mrow data-changed='added'>
		  <mi>f</mi>
		  <mo data-changed='added'>&#x2061;</mo>
		  <mrow data-changed='added'>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>+</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
		<mo>+</mo>
		<mrow data-changed='added'>
		  <mi>t</mi>
		  <mo data-changed='added' data-function-guess='true'>&#x2062;</mo>
		  <mrow data-changed='added'>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>+</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
		<mo>+</mo>
		<mrow data-changed='added'>
		  <mi>w</mi>
		  <mo data-changed='added'>&#x2061;</mo>
		  <mrow data-changed='added'>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>,</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
	  </mrow>
      </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn function_with_no_args() {
        let test_str = "<math><mrow>
		<mi>sin</mi><mi>x</mi>
			<mo>+</mo>
		 <mi>f</mi><mi>x</mi>
			<mo>+</mo>
		 <mi>t</mi><mi>x</mi>
		</mrow></math>";
        let target_str = " <math>
		<mrow>
		  <mrow data-changed='added'>
			<mi>sin</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mi>x</mi>
		  </mrow>
		  <mo>+</mo>
		  <mrow data-changed='added'>
			<mi>f</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
		  </mrow>
		  <mo>+</mo>
		  <mrow data-changed='added'>
			<mi>t</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));

	}


    #[test]
    fn function_call_vs_implied_times() {
        let test_str = "<math><mi>f</mi><mo>(</mo><mi>x</mi><mo>)</mo><mi>y</mi></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
				<mrow data-changed='added'>
					<mi>f</mi>
					<mo data-changed='added'>&#x2061;</mo>
					<mrow data-changed='added'> <mo>(</mo> <mi>x</mi> <mo>)</mo> </mrow>
				</mrow>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>y</mi>		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn implied_plus() {
        let test_str = "<math><mrow>
    <mn>2</mn><mfrac><mn>3</mn><mn>4</mn></mfrac>
    </mrow></math>";
        let target_str = "<math>
			<mrow>
				<mn>2</mn>
				<mo data-changed='added'>&#x2064;</mo>
				<mfrac>
					<mn>3</mn>
					<mn>4</mn>
				</mfrac>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn implied_plus_linear() {
        let test_str = "<math><mrow>
    <mn>2</mn><mn>3</mn><mo>/</mo><mn>4</mn>
    </mrow></math>";
        let target_str = "<math>
			<mrow>
				<mn>2</mn>
				<mo data-changed='added'>&#x2064;</mo>
				<mrow data-changed='added'>>
					<mn>3</mn>
					<mo>/</mo>
					<mn>4</mn>
				</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn implied_plus_linear2() {
        let test_str = "<math><mrow>
    <mn>2</mn><mrow><mn>3</mn><mo>/</mo><mn>4</mn></mrow>
    </mrow></math>";
        let target_str = "<math>
			<mrow>
				<mn>2</mn>
				<mo data-changed='added'>&#x2064;</mo>
				<mrow>
					<mn>3</mn>
					<mo>/</mo>
					<mn>4</mn>
				</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn implied_comma() {
        let test_str = "<math><msub><mi>b</mi><mrow><mn>1</mn><mn>2</mn></mrow></msub></math>";
        let target_str = "<math>
			 <msub><mi>b</mi><mrow><mn>1</mn><mo data-changed='added'>&#x2063;</mo><mn>2</mn></mrow></msub>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn no_implied_comma() {
        let test_str = "<math><mfrac><mi>b</mi><mrow><mn>1</mn><mn>2</mn></mrow></mfrac></math>";
        let target_str = "<math>
			 <mfrac><mi>b</mi><mrow><mn>1</mn><mo data-changed='added'>&#x2062;</mo><mn>2</mn></mrow></mfrac>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn vertical_bars() {
        let test_str = "<math>
		<mo>|</mo> <mi>x</mi> <mo>|</mo><mo>+</mo><mo>|</mo>
		 <mi>a</mi><mo>+</mo><mn>1</mn> <mo>|</mo>
	  </math>";
	  let target_str = " <math>
	  <mrow data-changed='added'>
		<mrow data-changed='added'>
		  <mo>|</mo>
		  <mi>x</mi>
		  <mo>|</mo>
		</mrow>
		<mo>+</mo>
		<mrow data-changed='added'>
		  <mo>|</mo>
		  <mrow data-changed='added'>
			<mi>a</mi>
			<mo>+</mo>
			<mn>1</mn>
		  </mrow>
		  <mo>|</mo>
		</mrow>
	  </mrow>
	 </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }


    #[test]
    fn vertical_bars_nested() {
        let test_str = "<math><mo>|</mo><mi>x</mi><mo>|</mo><mi>y</mi><mo>|</mo><mi>z</mi><mo>|</mo></math>";
	  let target_str = "<math>
	  <mrow data-changed='added'>
		<mrow data-changed='added'>
			<mo>|</mo>
			<mi>x</mi>
			<mo>|</mo>
		</mrow>
		<mo data-changed='added'>&#x2062;</mo>
		<mi>y</mi>
		<mo data-changed='added'>&#x2062;</mo>
		<mrow data-changed='added'>
			<mo>|</mo>
			<mi>z</mi>
			<mo>|</mo>
		</mrow>
	  </mrow>
	 </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn vertical_bar_such_that() {
        let test_str = "<math>
				<mo>{</mo><mi>x</mi><mo>|</mo><mi>x</mi><mo>&#x2208;</mo><mi>S</mi><mo>}</mo>
            </math>";
        let target_str = "<math>
		<mrow data-changed='added'>
		  <mo>{</mo>
		  <mrow data-changed='added'>
			<mi>x</mi>
			<mo>|</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>∈</mo>
			  <mi>S</mi>
			</mrow>
		  </mrow>
		  <mo>}</mo>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
	#[ignore]  // need to figure out a test for this ("|" should have a precedence around ":" since that is an alternative notation for "such that", but "∣" is higher precedence)
    fn vertical_bar_divides() {
        let test_str = "<math>
		<mi>x</mi><mo>+</mo><mi>y</mi> <mo>|</mo><mn>12</mn>
            </math>";
        let target_str = "<math>
				<mrow data-changed='added'>
				<mrow data-changed='added'>
					<mi>x</mi>
					<mo>+</mo>
					<mi>y</mi>
				</mrow>
				<mo>∣ <!--divides--></mo>
				<mn>12</mn>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }


    #[test]
    fn trig_mo() {
        let test_str = "<math><mo>sin</mo><mi>x</mi>
				<mo>+</mo><mo>cos</mo><mi>y</mi>
				<mo>+</mo><munder><mo>lim</mo><mi>D</mi></munder><mi>y</mi>
			</math>";
        let target_str = "<math>
		<mrow data-changed='added'>
		  <mrow data-changed='added'>
			<mi>sin</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mi>x</mi>
		  </mrow>
		  <mo>+</mo>
		  <mrow data-changed='added'>
			<mi>cos</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<mi>y</mi>
		  </mrow>
		  <mo>+</mo>
		  <mrow data-changed='added'>
			<munder>
			  <mi>lim</mi>
			  <mi>D</mi>
			</munder>
			<mo data-changed='added'>&#x2061;</mo>
			<mi>y</mi>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }
	
    #[test]
    fn trig_negative_args() {
        let test_str = "<math><mi>sin</mi><mo>-</mo><mn>2</mn><mi>π</mi><mi>x</mi></math>";
        let target_str = "<math>
		<mrow data-changed='added'>
		  <mi>sin</mi>
		  <mo data-changed='added'>&#x2061;</mo>
		  <mrow data-changed='added'>
			<mrow data-changed='added'>
			  <mo>-</mo>
			  <mn>2</mn>
			</mrow>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>π</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }
	
    #[test]
    fn not_trig_negative_args() {
		// this is here to make sure that only trig functions get the special treatment
        let test_str = "<math><mi>ker</mi><mo>-</mo><mn>2</mn><mi>π</mi><mi>x</mi></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
					<mrow data-changed='added'>
					<mi>ker</mi>
					<mo data-changed='added'>&#x2061;</mo>
					<mrow data-changed='added'>
						<mo>-</mo>
						<mn>2</mn>
					</mrow>
					</mrow>
				<mo data-changed='added'>&#x2062;</mo>
				<mi>π</mi>
				<mo data-changed='added'>&#x2062;</mo>
				<mi>x</mi>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn trig_args() {
        let test_str = "<math><mi>sin</mi><mn>2</mn><mi>π</mi><mi>x</mi></math>";
        let target_str = "<math>
		<mrow data-changed='added'>
		  <mi>sin</mi>
		  <mo data-changed='added'>&#x2061;</mo>
		  <mrow data-changed='added'>
			<mn>2</mn>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>π</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn not_trig_args() {
		// this is here to make sure that only trig functions get the special treatment
        let test_str = "<math><mi>ker</mi><mn>2</mn><mi>π</mi><mi>x</mi></math>";
        let target_str = "<math>
		<mrow data-changed='added'>
			<mrow data-changed='added'>
				<mi>ker</mi>
				<mo data-changed='added'>&#x2061;</mo>
				<mn>2</mn>
			</mrow>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>π</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn trig_trig() {
        let test_str = "<math><mi>sin</mi><mi>x</mi><mi>cos</mi><mi>y</mi></math>";
        let target_str = "<math>
		<mrow data-changed='added'>
			<mrow data-changed='added'>
				<mi>sin</mi>
				<mo data-changed='added'>&#x2061;</mo>
				<mi>x</mi>
			</mrow>
			<mo data-changed='added'>&#x2062;</mo>
			<mrow data-changed='added'>
				<mi>cos</mi>
				<mo data-changed='added'>&#x2061;</mo>
				<mi>y</mi>
			</mrow>
		</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

    #[test]
    fn trig_function_composition() {
        let test_str = "<math><mo>(</mo><mi>sin</mi><mo>-</mo><mi>cos</mi><mo>)</mo><mi>x</mi></math>";
        let target_str = "<math>
		<mrow data-changed='added'>
		  <mrow data-changed='added'>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>sin</mi>
			  <mo>-</mo>
			  <mi>cos</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		  <mo data-changed='added'>&#x2062;</mo>
		  <mi>x</mi>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
    }

	
	#[test]
    fn mtext_whitespace_string() {
        let test_str = "<math><mi>t</mi><mtext>&#x00A0;&#x205F;</mtext></math>";
        let target_str = "<math><mi>t&#x00A0;</mi></math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}
	
	#[test]
    fn mtext_whitespace_string_before() {
        let test_str = "<math><mtext>&#x00A0;&#x205F;</mtext><mi>t</mi></math>";
        let target_str = "<math><mi>&#x00A0;t</mi></math>";
		assert!(are_strs_canonically_equal(test_str, target_str));
	}
	
	#[test]
    fn mtext_whitespace_1() {
        let test_str = "<math><mi>t</mi><mtext>&#x00A0;&#x205F;</mtext>
				<mrow><mo>(</mo><mi>x</mi><mo>+</mo><mi>y</mi><mo>)</mo></mrow></math>";
        let target_str = " <math>
		<mrow data-changed='added'>
		  <mi>t&#x00A0;</mi>
		  <mo data-changed='added' data-function-guess='true'>&#x2062;</mo>
		  <mrow>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>+</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}
	
	#[test]
    fn mtext_whitespace_2() {
        let test_str = "<math><mi>f</mi><mtext>&#x00A0;&#x205F;</mtext>
				<mrow><mo>(</mo><mi>x</mi><mo>+</mo><mi>y</mi><mo>)</mo></mrow></math>";
        let target_str = " <math>
		<mrow data-changed='added'>
		  <mi>f&#x00A0;</mi>
		  <mo data-changed='added'>&#x2061;</mo>
		  <mrow>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>+</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn remove_mtext_whitespace_3() {
        let test_str = "<math><mi>t</mi>
				<mrow><mtext>&#x2009;</mtext><mo>(</mo><mi>x</mi><mo>+</mo><mi>y</mi><mo>)</mo></mrow></math>";
        let target_str = "<math>
		<mrow data-changed='added'>
		  <mi>t</mi>
		  <mo data-changed='added' data-function-guess='true'>&#x2062;</mo>
		  <mrow>
			<mo>(</mo>
			<mrow data-changed='added'>
			  <mi>x</mi>
			  <mo>+</mo>
			  <mi>y</mi>
			</mrow>
			<mo>)</mo>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn do_not_remove_any_whitespace() {
        let test_str = "<math><mfrac>
					<mrow><mspace width='3em'/></mrow>
					<mtext>&#x2009;</mtext>
				</mfrac></math>";
        let target_str = " <math> <mfrac>
		  <mtext width='3em' data-changed='empty_content'> </mtext>
		  <mtext data-changed='empty_content'> </mtext>
		</mfrac> </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn remove_mo_whitespace() {
        let test_str = "<math><mi>cos</mi><mo>&#xA0;</mo><mi>x</mi></math>";
        let target_str = "<math>
				<mrow data-changed='added'>
					<mi>cos&#xA0;</mi>
					<mo data-changed='added'>&#x2061;</mo>
					<mi>x</mi>
				</mrow>
	  		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn do_not_remove_some_whitespace() {
        let test_str = "<math><mroot>
					<mrow><mi>b</mi><mphantom><mi>y</mi></mphantom></mrow>
					<mtext>&#x2009;</mtext>
				</mroot></math>";
        let target_str = "<math><mroot>
				<mi>b</mi>
				<mtext data-changed='empty_content'>&#xA0;</mtext>
			</mroot></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn remove_all_extra_elements() {
        let test_str = "<math><msqrt>
					<mstyle> <mi>b</mi> </mstyle>
					<mphantom><mi>y</mi></mphantom>
					<mtext>&#x2009;</mtext>
					<mspace width='3em'/>
				</msqrt></math>";
        let target_str = "<math><msqrt>
				<mi>b&#xA0;</mi>
			</msqrt></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn empty_content() {
        let test_str = "<math></math>";
        let target_str = " <math><mtext data-added='missing-content' data-changed='empty_content'> </mtext></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn empty_content_after_cleanup() {
        let test_str = "<math><mrow><mphantom><mn>1</mn></mphantom></mrow></math>";
        let target_str = " <math><mtext data-added='missing-content'> </mtext></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}


	#[test]
    fn clean_semantics() {
		// this comes from LateXML
        let test_str = "<math>
				<semantics>
					<mrow><mi>z</mi></mrow>
					<annotation-xml encoding='MathML-Content'>
						<ci>𝑧</ci>
					</annotation-xml>
					<annotation encoding='application/x-tex'>z</annotation>
					<annotation encoding='application/x-llamapun'>italic_z</annotation>
				</semantics>
			</math>";
		let target_str = "<math>
		<semantics>
			<mi>z</mi>
			<annotation-xml encoding='MathML-Content'>
				<ci>𝑧</ci>
			</annotation-xml>
			<annotation encoding='application/x-tex'>z</annotation>
			<annotation encoding='application/x-llamapun'>italic_z</annotation>
		</semantics>
	</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn clean_up_mi_operator() {
        let test_str = "<math><mrow><mi>∠</mi><mi>A</mi><mi>B</mi><mi>C</mi></mrow></math>";
        let target_str = " <math>
				<mrow>
				<mo>∠</mo>
				<mrow data-changed='added'>
					<mi>A</mi>
					<mo data-changed='added'>&#x2063;</mo>
					<mi>B</mi>
					<mo data-changed='added'>&#x2063;</mo>
					<mi>C</mi>
				</mrow>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}


	#[test]
    fn clean_up_arc() {
        let test_str = "<math><mtext>arc&#xA0;</mtext><mi>cos</mi><mi>x</mi></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
			<mi>arccos</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn clean_up_arc_nospace() {
        let test_str = "<math><mtext>arc</mtext><mi>cos</mi><mi>x</mi></math>";
        let target_str = "<math>
			<mrow data-changed='added'>
			<mi>arccos</mi>
			<mo data-changed='added'>&#x2062;</mo>
			<mi>x</mi>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn roman_numeral() {
        let test_str = "<math><mrow><mtext>XLVIII</mtext> <mo>+</mo><mn>mmxxvi</mn></mrow></math>";
		// turns out there is no need to mark them as Roman Numerals -- thought that was need for braille
        // let target_str = "<math><mrow>
		// 	<mn data-roman-numeral='true'>XLVIII</mn> <mo>+</mo><mn data-roman-numeral='true'>mmxxvi</mn>
		// 	</mrow></math>";
        let target_str = "<math><mrow><mtext>XLVIII</mtext> <mo>+</mo><mn>mmxxvi</mn></mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	// #[test]
    // fn roman_numeral_context() {
    //     let test_str = "<math><mi>vi</mi><mo>-</mo><mi mathvariant='normal'>i</mi><mo>=</mo><mtext>v</mtext></math>";
    //     let target_str = "<math> <mrow data-changed='added'>
	// 		<mrow data-changed='added'><mn data-roman-numeral='true'>vi</mn><mo>-</mo><mn mathvariant='normal' data-roman-numeral='true'>i</mn></mrow> 
	// 		<mo>=</mo> <mn data-roman-numeral='true'>v</mn>
	// 	</mrow> </math>";
    //     assert!(are_strs_canonically_equal(test_str, target_str));
	// }

	// #[test]
    // fn not_roman_numeral() {
    //     let test_str = "<math><mtext>cm</mtext></math>";
	// 	// shouldn't change
    //     let target_str = "<math><mtext>cm</mtext></math>";
    //     assert!(are_strs_canonically_equal(test_str, target_str));
	// }

	#[test]
    fn digit_block_binary() {
        let test_str = "<math><mo>(</mo><mn>0110</mn><mspace width=\"thickmathspace\"></mspace><mn>1110</mn><mspace width=\"thickmathspace\"></mspace><mn>0110</mn><mo>)</mo></math>";
        let target_str = " <math>
				<mrow data-changed='added'>
				<mo>(</mo>
				<mn>0110\u{A0}1110\u{A0}0110</mn>
				<mo>)</mo>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn digit_block_decimal() {
        let test_str = "<math><mn>8</mn><mo>,</mo><mn>123</mn><mo>,</mo><mn>456</mn><mo>+</mo>
								    <mn>4</mn><mo>.</mo><mn>32</mn></math>";
        let target_str = " <math>
				<mrow data-changed='added'>
				<mn>8,123,456</mn>
				<mo>+</mo>
				<mn>4.32</mn>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
	fn digit_block_int() {
        let test_str = "<math><mn>12</mn><mo>,</mo><mn>345</mn><mo>+</mo>
								    <mn>1</mn><mo>,</mo><mn>000</mn></math>";
        let target_str = " <math>
				<mrow data-changed='added'>
				<mn>12,345</mn>
				<mo>+</mo>
				<mn>1,000</mn>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn digit_block_decimal_pt() {
        let test_str = "<math><mn>8</mn><mo>,</mo><mn>123</mn><mo>.</mo>
								<mo>+</mo><mn>4</mn><mo>.</mo>
								<mo>+</mo><mo>.</mo><mn>01</mn></math>";
        let target_str = " <math>
				<mrow data-changed='added'>
				<mn>8,123.</mn>
				<mo>+</mo>
				<mn>4.</mn>
				<mo>+</mo>
				<mn>.01</mn>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn not_digit_block_parens() {
        let test_str = "<math><mo>(</mo><mn>451</mn><mo>,</mo><mn>231</mn><mo>)</mo></math>";
        let target_str = " <math> <mrow data-changed='added'>
				<mo>(</mo>
				<mrow data-changed='added'>
				<mn>451</mn> <mo>,</mo> <mn>231</mn>
				</mrow>
				<mo>)</mo>
			</mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn not_digit_block_parens_mrow() {
        let test_str = "<math><mo>(</mo><mrow><mn>451</mn><mo>,</mo><mn>231</mn></mrow><mo>)</mo></math>";
        let target_str = " <math> <mrow data-changed='added'>
				<mo>(</mo>
				<mrow>
				<mn>451</mn> <mo>,</mo> <mn>231</mn>
				</mrow>
				<mo>)</mo>
			</mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn not_digit_block_decimal() {
        let test_str = "<math><mn>8</mn><mo>,</mo><mn>49</mn><mo>,</mo><mn>456</mn><mo>+</mo>
								    <mn>4</mn><mtext> </mtext><mn>32</mn><mo>+</mo>
									<mn>1</mn><mo>,</mo><mn>234</mn><mo>,</mo><mn>56</mn></math>";
        let target_str = "  <math>
				<mrow data-changed='added'>
				<mn>8</mn>
				<mo>,</mo>
				<mn>49</mn>
				<mo>,</mo>
				<mrow data-changed='added'>
					<mn>456</mn>
					<mo>+</mo>
					<mrow data-changed='added'>
					<mn>4</mn>
					<mo data-changed='added'>&#x2062;</mo>
					<mn>32</mn>
					</mrow>
					<mo>+</mo>
					<mn>1</mn>
				</mrow>
				<mo>,</mo>
				<mn>234</mn>
				<mo>,</mo>
				<mn>56</mn>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn not_digit_block_ellipsis() {
        let test_str = "<math><mrow><mn>8</mn><mo>,</mo><mn>123</mn><mo>,</mo><mn>456</mn><mo>,</mo>
								    <mi>…</mi></mrow></math>";
        let target_str = "<math>
		<mrow>
		  <mn>8</mn>
		  <mo>,</mo>
		  <mn>123</mn>
		  <mo>,</mo>
		  <mn>456</mn>
		  <mo>,</mo>
		  <mi>…</mi>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn ellipsis() {
        let test_str = "<math><mn>5</mn><mo>,</mo><mo>.</mo><mo>.</mo><mo>.</mo><mo>,</mo><mn>8</mn><mo>,</mo>
				<mn>9</mn><mo>,</mo><mo>.</mo><mo>.</mo><mo>.</mo><mo>,</mo><mn>11</mn><mo>,</mo>
				<mn>5</mn><mo>,</mo><mo>.</mo><mo>.</mo><mo>,</mo><mn>8</mn>
			</math>";
        let target_str = "<math><mrow data-changed='added'>
			<mn>5</mn><mo>,</mo><mi>…</mi><mo>,</mo><mn>8</mn><mo>,</mo>
			<mn>9</mn><mo>,</mo><mi>…</mi><mo>,</mo><mn>11</mn><mo>,</mo>
			<mn>5</mn><mo>,</mo><mrow data-changed='added'><mo>.</mo><mo>.</mo></mrow>
			<mo>,</mo><mn>8</mn></mrow></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn primes_common() {
        let test_str = "<math><msup><mn>5</mn><mo>'</mo></msup>
							<msup><mn>5</mn><mo>''</mo></msup>
							<msup><mn>8</mn><mrow><mo>'</mo><mo>'</mo></mrow></msup></math>";
        let target_str = "<math>
				<mrow data-changed='added'>
				<msup>
					<mn>5</mn>
					<mo>′</mo>
				</msup>
				<mo data-changed='added'>&#x2062;</mo>
				<msup>
					<mn>5</mn>
					<mo>″</mo>
				</msup>
				<mo data-changed='added'>&#x2062;</mo>
				<msup>
					<mn>8</mn>
					<mo>″</mo>
				</msup>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn primes_uncommon() {
        let test_str = "<math><msup><mn>5</mn><mo>''′</mo></msup>
							<msup><mn>5</mn><mo>''''</mo></msup>
							<msup><mn>8</mn><mrow><mo>′</mo><mo>⁗</mo></mrow></msup></math>";
        let target_str = " <math>
				<mrow data-changed='added'>
				<msup>
					<mn>5</mn>
					<mo>‴</mo>
				</msup>
				<mo data-changed='added'>&#x2062;</mo>
				<msup>
					<mn>5</mn>
					<mo>⁗</mo>
				</msup>
				<mo data-changed='added'>&#x2062;</mo>
				<msup>
					<mn>8</mn>
					<mo>⁗′</mo>
				</msup>
				</mrow>
			</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn parent_bug_94() {
		// Note: this isn't ideal -- it really should merge the leading '0' to get just one mn with content "0.02"
        let test_str = "	<math>
		<mrow>
			<msqrt>
				<mrow>
					<mstyle mathvariant='bold' mathsize='normal'><mn>0</mn></mstyle>
					<mstyle mathvariant='bold' mathsize='normal'><mo>.</mo><mn>0</mn><mn>2</mn></mstyle>
				</mrow>
			</msqrt>
		</mrow>
	</math>
	";
        let target_str = "<math>
		<msqrt>
		  <mrow>
			<mn mathsize='normal' mathvariant='bold'>𝟎</mn>
			<mo data-changed='added'>&#x2062;</mo>
			<mn mathsize='normal' mathvariant='bold' data-changed='added'>.02</mn>
		  </mrow>
		</msqrt>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn lift_script() {
        let test_str = "<math xmlns='http://www.w3.org/1998/Math/MathML' >
		<mrow>
		  <mstyle scriptlevel='0' displaystyle='true'>
			<mrow>
			  <msqrt>
				<munder>
				  <mo>∑<!-- ∑ --></mo>
				  <mrow>
					<mn>0</mn>
					<mo>≤<!-- ≤ --></mo>
					<mi>k</mi>
					<mo>≤<!-- ≤ --></mo>
					<mi>n</mi>
				  </mrow>
				</munder>
				<mrow>
				  <mo stretchy='false'>|</mo>
				</mrow>
				<msub>
				  <mi>a</mi>
				  <mrow>
					<mi>k</mi>
				  </mrow>
				</msub>
				<msup>
				  <mrow>
					<mo stretchy='false'>|</mo>
				  </mrow>
				  <mrow>
					<mn>2</mn>
				  </mrow>
				</msup>
			  </msqrt>
			</mrow>
		  </mstyle>
		</mrow>
	  </math>";
        let target_str = "<math>
		<msqrt scriptlevel='0' displaystyle='true'>
		  <mrow data-changed='added'>
			<munder>
			  <mo>∑</mo>
			  <mrow>
				<mn>0</mn>
				<mo>≤</mo>
				<mi>k</mi>
				<mo>≤</mo>
				<mi>n</mi>
			  </mrow>
			</munder>
			<msup>
			  <mrow data-changed='added'>
				<mo stretchy='false'>|</mo>
				<msub>
				  <mi>a</mi>
				  <mi>k</mi>
				</msub>
				<mo stretchy='false'>|</mo>
			  </mrow>
			  <mn>2</mn>
			</msup>
		  </mrow>
		</msqrt>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn pseudo_scripts() {
        let test_str = "<math><mrow>
				<mi>cos</mi><mn>30</mn><mo>°</mo>
				<mi>sin</mi><mn>60</mn><mo>′</mo>
				</mrow></math>";
        let target_str = "<math>
		<mrow>
		  <mrow data-changed='added'>
			<mi>cos</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<msup data-changed='added'><mn>30</mn><mo>°</mo></msup>
		  </mrow>
		  <mo data-changed='added'>&#x2062;</mo>
		  <mrow data-changed='added'>
			<mi>sin</mi>
			<mo data-changed='added'>&#x2061;</mo>
			<msup data-changed='added'><mn>60</mn><mo>′</mo></msup>
		  </mrow>
		</mrow>
	   </math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn prescript_only() {
        let test_str = "<math><msub><mtext/><mn>92</mn></msub><mi>U</mi></math>";
        let target_str = "<math><mmultiscripts><mi>U</mi><mprescripts/> <mn>92</mn><none/> </mmultiscripts></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn pre_and_postscript_only() {
        let test_str = "<math>
			<msub><mrow/><mn>0</mn></msub>
			<msub><mi>F</mi><mn>1</mn></msub>
			<mo stretchy='false'>(</mo>
			<mi>a</mi><mo>,</mo><mi>b</mi><mo>;</mo><mi>c</mi><mo>;</mo><mi>z</mi>
			<mo stretchy='false'>)</mo>
		</math>";
			let target_str = " <math>
			<mrow data-changed='added'>
			<mmultiscripts>
				<mi>F</mi>
				<mn>1</mn>
				<none></none>
				<mprescripts></mprescripts>
				<mn>0</mn>
				<none></none>
			</mmultiscripts>
			<mo data-changed='added'>&#x2061;</mo>
			<mrow data-changed='added'>
				<mo stretchy='false'>(</mo>
				<mrow data-changed='added'>
				<mrow data-changed='added'>
					<mi>a</mi>
					<mo>,</mo>
					<mi>b</mi>
				</mrow>
				<mo>;</mo>
				<mi>c</mi>
				<mo>;</mo>
				<mi>z</mi>
				</mrow>
				<mo stretchy='false'>)</mo>
			</mrow>
			</mrow>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}

	#[test]
    fn pointless_nones_in_mmultiscripts() {
        let test_str = "<math><mmultiscripts>
				<mtext>C</mtext>
				<none />
				<none />
				<mprescripts />
				<mn>6</mn>
				<mn>14</mn>
			</mmultiscripts></math>";
        let target_str = "<math><mmultiscripts>
				<mtext>C</mtext>
				<mprescripts />
				<mn>6</mn>
				<mn>14</mn>
			</mmultiscripts></math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}


	#[test]
	#[ignore]	// this fails -- need to figure out grabbing base from previous or next child
    fn tensor() {
        let test_str = "<math>
				<msub><mi>R</mi><mi>i</mi></msub>
				<msup><mrow/><mi>j</mi></msup>
				<msub><mrow/><mi>k</mi></msub>
				<msub><mrow/><mi>l</mi></msub>
			</math>";
		let target_str = "<math>
			<mmultiscripts>
				<mi> R </mi>
				<mi> i </mi>
				<none/>
				<none/>
				<mi> j </mi>
				<mi> k </mi>
				<none/>
				<mi> l </mi>
				<none/>
			</mmultiscripts>
		</math>";
        assert!(are_strs_canonically_equal(test_str, target_str));
	}
}

