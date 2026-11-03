"""Hand-written phrasings for the synthetic Ask generator in synth.py. Each class lists its plural
noun and, for each relation, four phrasings: the question for the value of the relation
of an entity {e}, the predicate that selects instances having value {o}, the yes or no
question, and a noun phrase for the value that a two-hop question builds on."""

CLASSES = {
    'Film': ('film', 'films', {
        'director': ('Who directed {e}?', 'were directed by {o}', 'Was {e} directed by {o}?', 'the director of {e}'),
        'starring': ('Who starred in {e}?', 'starred {o}', 'Did {o} star in {e}?', 'the star of {e}'),
        'writer': ('Who wrote {e}?', 'were written by {o}', 'Was {e} written by {o}?', 'the writer of {e}'),
        'producer': ('Who produced {e}?', 'were produced by {o}', 'Was {e} produced by {o}?', 'the producer of {e}'),
        'musicComposer': ('Who composed the music for {e}?', 'have music by {o}', 'Did {o} compose the music for {e}?', 'the composer of {e}'),
        'distributor': ('Which company distributed {e}?', 'were distributed by {o}', 'Was {e} distributed by {o}?', 'the distributor of {e}'),
    }),
    'Book': ('book', 'books', {
        'author': ('Who wrote {e}?', 'were written by {o}', 'Was {e} written by {o}?', 'the author of {e}'),
        'publisher': ('Who published {e}?', 'were published by {o}', 'Was {e} published by {o}?', 'the publisher of {e}'),
        'literaryGenre': ('What genre is the book {e}?', 'belong to the genre {o}', 'Is {e} a work of {o}?', None),
    }),
    'City': ('city', 'cities', {
        'country': ('In which country is {e}?', 'are in {o}', 'Is {e} in {o}?', 'the country of {e}'),
        'subdivision': ('In which region is {e}?', 'are in the region {o}', 'Is {e} in {o}?', None),
        'timeZone': ('What time zone is {e} in?', 'are in the {o}', 'Is {e} in the {o}?', None),
        'politicalLeader': ('Who leads the city of {e}?', 'are led by {o}', 'Is {o} the leader of {e}?', 'the leader of {e}'),
        'namedAfter': ('Who or what is {e} named after?', 'are named after {o}', 'Is {e} named after {o}?', None),
    }),
    'Country': ('country', 'countries', {
        'capital': ('What is the capital of {e}?', 'have {o} as their capital', 'Is {o} the capital of {e}?', 'the capital of {e}'),
        'currency': ('What is the currency of {e}?', 'use the {o}', 'Is the {o} the currency of {e}?', None),
        'language': ('Which languages are spoken in {e}?', 'have {o} as a language', 'Is {o} spoken in {e}?', None),
        'governmentType': ('What form of government does {e} have?', 'have the government type {o}', 'Is {e} a {o}?', None),
    }),
    'Company': ('company', 'companies', {
        'industry': ('In which industry does {e} operate?', 'are in the {o} industry', 'Is {e} in the {o} industry?', None),
        'product': ('What products does {e} make?', 'make {o}', 'Does {e} make {o}?', None),
        'keyPerson': ('Who are the key people at {e}?', 'have {o} as a key person', 'Is {o} a key person at {e}?', 'a key person at {e}'),
        'parentCompany': ('What is the parent company of {e}?', 'are owned by {o}', 'Is {o} the parent company of {e}?', 'the parent company of {e}'),
        'location': ('Where is {e} based?', 'are based in {o}', 'Is {e} based in {o}?', None),
    }),
    'Band': ('band', 'bands', {
        'genre': ('What genre of music does {e} play?', 'play {o}', 'Does {e} play {o}?', None),
        'hometown': ('Where is the band {e} from?', 'come from {o}', 'Is {e} from {o}?', 'the hometown of {e}'),
        'recordLabel': ('Which record labels has {e} been signed to?', 'were signed to {o}', 'Was {e} signed to {o}?', 'the record label of {e}'),
        'formerBandMember': ('Who are the former members of {e}?', 'had {o} as a member', 'Was {o} a member of {e}?', 'a former member of {e}'),
    }),
    'University': ('university', 'universities', {
        'city': ('In which city is {e}?', 'are in {o}', 'Is {e} in {o}?', 'the city of {e}'),
        'country': ('In which country is {e}?', 'are in {o}', 'Is {e} in {o}?', 'the country of {e}'),
        'affiliation': ('Which organizations is {e} affiliated with?', 'are affiliated with {o}', 'Is {e} affiliated with {o}?', None),
    }),
    'River': ('river', 'rivers', {
        'riverMouth': ('Into what does the {e} flow?', 'flow into {o}', 'Does the {e} flow into {o}?', None),
        'country': ('Through which countries does the {e} flow?', 'flow through {o}', 'Does the {e} flow through {o}?', None),
        'mouthPlace': ('Where is the mouth of the {e}?', 'have their mouth in {o}', 'Is the mouth of the {e} in {o}?', None),
    }),
    'Mountain': ('mountain', 'mountains', {
        'mountainRange': ('Which mountain range is {e} part of?', 'are in the {o}', 'Is {e} in the {o}?', 'the mountain range of {e}'),
        'country': ('In which country is {e}?', 'are in {o}', 'Is {e} in {o}?', 'the country of {e}'),
        'locatedInArea': ('Where is {e} located?', 'are located in {o}', 'Is {e} located in {o}?', None),
    }),
    'Politician': ('politician', 'politicians', {
        'party': ('Which party does {e} belong to?', 'belong to the {o}', 'Is {e} a member of the {o}?', 'the party of {e}'),
        'birthPlace': ('Where was {e} born?', 'were born in {o}', 'Was {e} born in {o}?', 'the birthplace of {e}'),
        'almaMater': ('Where did {e} study?', 'studied at {o}', 'Did {e} study at {o}?', 'the alma mater of {e}'),
        'deathPlace': ('Where did {e} die?', 'died in {o}', 'Did {e} die in {o}?', 'the place where {e} died'),
    }),
    'Scientist': ('scientist', 'scientists', {
        'almaMater': ('Where did {e} study?', 'studied at {o}', 'Did {e} study at {o}?', 'the alma mater of {e}'),
        'birthPlace': ('Where was {e} born?', 'were born in {o}', 'Was {e} born in {o}?', 'the birthplace of {e}'),
        'institution': ('At which institutions did {e} work?', 'worked at {o}', 'Did {e} work at {o}?', None),
        'award': ('Which awards did {e} receive?', 'received the {o}', 'Did {e} receive the {o}?', None),
        'academicDiscipline': ('What field did {e} work in?', 'worked in {o}', 'Did {e} work in {o}?', None),
    }),
}

# The second hop of a two-hop question, keyed by property, over a noun phrase {x}.
HOPS = {
    'birthPlace': 'Where was {x} born?',
    'deathPlace': 'Where did {x} die?',
    'almaMater': 'Where did {x} study?',
    'country': 'In which country is {x}?',
    'capital': 'What is the capital of {x}?',
    'party': 'Which party does {x} belong to?',
    'nationality': 'What is the nationality of {x}?',
    'spouse': 'Who is the spouse of {x}?',
    'headquarter': 'Where is {x} headquartered?',
    'location': 'Where is {x} based?',
    'founder': 'Who founded {x}?',
    'foundedBy': 'Who founded {x}?',
    'leaderName': 'Who leads {x}?',
    'currency': 'What is the currency of {x}?',
}

# Superlatives as (group relation, number, question over the group value {g}, order).
SUPERLATIVES = {
    'Film': [('director', 'runtime', 'Which film directed by {g} is the longest?', 'DESC'),
             ('starring', 'runtime', 'Which film starring {g} is the longest?', 'DESC')],
    'Book': [('author', 'numberOfPages', 'Which book by {g} has the most pages?', 'DESC')],
    'City': [('country', 'populationTotal', 'Which city in {g} has the largest population?', 'DESC'),
             ('country', 'elevation', 'Which city in {g} lies highest?', 'DESC')],
    'River': [('country', 'length', 'What is the longest river in {g}?', 'DESC')],
    'Mountain': [('mountainRange', 'elevation', 'What is the highest mountain in the {g}?', 'DESC'),
                 ('country', 'elevation', 'What is the highest mountain in {g}?', 'DESC')],
    'University': [('country', 'numberOfStudents', 'Which university in {g} has the most students?', 'DESC')],
    'Company': [('industry', 'numberOfEmployees', 'Which company in the {g} industry has the most employees?', 'DESC')],
    'Politician': [('party', 'birthDate', 'Who is the oldest politician of the {g}?', 'ASC')],
}
