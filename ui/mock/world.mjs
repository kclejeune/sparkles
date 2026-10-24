// The mock's `world` dataset, a showcase for the README's screenshots of the maps of
// relations and of the GraphRAG communities: Meridian Logistics, an invented freight
// company with its head office in Dubai, its offices in a dozen cities, five partner
// companies, 41 people in 30 cities on six continents who work for them and know each
// other, and 37 freight routes between the cities. Every person and company is invented,
// and the cities are real with their real coordinates.
//
// The GraphRAG communities of the dataset (WORLD_COMMUNITIES, read by mock/graphrag.mjs)
// group the people by region in three levels, with a canned report for each community.

export const WORLD = 'http://example.org/world/';

const PREFIXES = `@prefix ex: <${WORLD}> .
@prefix geo: <http://www.opengis.net/ont/geosparql#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
`;

/** id, label, longitude, latitude */
const CITIES = [
  ['dubai', 'Dubai', 55.2708, 25.2048],
  ['london', 'London', -0.1276, 51.5072],
  ['paris', 'Paris', 2.3522, 48.8566],
  ['frankfurt', 'Frankfurt', 8.6821, 50.1109],
  ['madrid', 'Madrid', -3.7038, 40.4168],
  ['istanbul', 'Istanbul', 28.9784, 41.0082],
  ['cairo', 'Cairo', 31.2357, 30.0444],
  ['nairobi', 'Nairobi', 36.8219, -1.2921],
  ['lagos', 'Lagos', 3.3792, 6.5244],
  ['johannesburg', 'Johannesburg', 28.0473, -26.2041],
  ['casablanca', 'Casablanca', -7.5898, 33.5731],
  ['mumbai', 'Mumbai', 72.8777, 19.076],
  ['delhi', 'Delhi', 77.1025, 28.7041],
  ['singapore', 'Singapore', 103.8198, 1.3521],
  ['bangkok', 'Bangkok', 100.5018, 13.7563],
  ['jakarta', 'Jakarta', 106.8456, -6.2088],
  ['hongkong', 'Hong Kong', 114.1694, 22.3193],
  ['shanghai', 'Shanghai', 121.4737, 31.2304],
  ['seoul', 'Seoul', 126.978, 37.5665],
  ['tokyo', 'Tokyo', 139.6917, 35.6895],
  ['sydney', 'Sydney', 151.2093, -33.8688],
  ['melbourne', 'Melbourne', 144.9631, -37.8136],
  ['auckland', 'Auckland', 174.7633, -36.8485],
  ['newyork', 'New York', -74.006, 40.7128],
  ['toronto', 'Toronto', -79.3832, 43.6532],
  ['chicago', 'Chicago', -87.6298, 41.8781],
  ['mexicocity', 'Mexico City', -99.1332, 19.4326],
  ['losangeles', 'Los Angeles', -118.2437, 34.0522],
  ['vancouver', 'Vancouver', -123.1207, 49.2827],
  ['saopaulo', 'São Paulo', -46.6333, -23.5505],
  ['buenosaires', 'Buenos Aires', -58.3816, -34.6037],
  ['bogota', 'Bogotá', -74.0721, 4.711],
  ['lima', 'Lima', -77.0428, -12.0464],
  ['santiago', 'Santiago', -70.6693, -33.4489],
];
const CITY = new Map(CITIES.map(([id, label, lon, lat]) => [id, { label, lon, lat }]));

/** id, label, head office */
const COMPANIES = [
  ['meridian', 'Meridian Logistics', 'dubai'],
  ['harbourline', 'Harbour Line Shipping', 'hongkong'],
  ['northgate', 'Northgate Rail', 'chicago'],
  ['andes', 'Andes Cargo', 'lima'],
  ['bosphorus', 'Bosphorus Air Cargo', 'istanbul'],
  ['southerncross', 'Southern Cross Ports', 'melbourne'],
];

const OFFICES = [
  'london',
  'frankfurt',
  'singapore',
  'shanghai',
  'tokyo',
  'mumbai',
  'johannesburg',
  'lagos',
  'newyork',
  'mexicocity',
  'saopaulo',
  'sydney',
];

/** id, label, city, employer, role */
const PEOPLE = [
  ['amira-haddad', 'Amira Haddad', 'dubai', 'meridian', 'Chief executive'],
  ['omar-rahman', 'Omar Rahman', 'dubai', 'meridian', 'Operations director'],
  ['nadia-farouk', 'Nadia Farouk', 'cairo', 'meridian', 'Customs lead'],
  ['priya-nair', 'Priya Nair', 'mumbai', 'meridian', 'Port manager'],
  ['arjun-mehta', 'Arjun Mehta', 'delhi', 'meridian', 'Network planner'],
  ['sara-lindgren', 'Sara Lindgren', 'london', 'meridian', 'Head of Europe'],
  ['tom-ashworth', 'Tom Ashworth', 'london', 'meridian', 'Account director'],
  ['claire-dubois', 'Claire Dubois', 'paris', 'meridian', 'Compliance officer'],
  ['jonas-weber', 'Jonas Weber', 'frankfurt', 'meridian', 'Hub manager'],
  ['lucia-moreno', 'Lucía Moreno', 'madrid', 'meridian', 'Route analyst'],
  ['emre-kaya', 'Emre Kaya', 'istanbul', 'meridian', 'Air freight lead'],
  ['wanjiru-mwangi', 'Wanjiru Mwangi', 'nairobi', 'meridian', 'Regional manager'],
  ['tunde-adebayo', 'Tunde Adebayo', 'lagos', 'meridian', 'Port manager'],
  ['thandi-nkosi', 'Thandi Nkosi', 'johannesburg', 'meridian', 'Head of Africa'],
  ['wei-lin-tan', 'Wei Lin Tan', 'singapore', 'meridian', 'Head of Asia-Pacific'],
  ['siti-rahma', 'Siti Rahma', 'jakarta', 'meridian', 'Sales lead'],
  ['chen-jie', 'Chen Jie', 'shanghai', 'meridian', 'Hub manager'],
  ['min-jun-park', 'Min-jun Park', 'seoul', 'meridian', 'Account director'],
  ['yuki-sato', 'Yuki Sato', 'tokyo', 'meridian', 'Head of North Asia'],
  ['liam-carter', 'Liam Carter', 'sydney', 'meridian', 'Head of Oceania'],
  ['grace-walker', 'Grace Walker', 'newyork', 'meridian', 'Head of Americas'],
  ['diego-alvarez', 'Diego Álvarez', 'mexicocity', 'meridian', 'Hub manager'],
  ['camila-souza', 'Camila Souza', 'saopaulo', 'meridian', 'Head of Latin America'],
  ['mateo-rojas', 'Mateo Rojas', 'bogota', 'meridian', 'Route analyst'],
  ['kai-wong', 'Kai Wong', 'hongkong', 'harbourline', 'Fleet director'],
  ['mei-chan', 'Mei Chan', 'hongkong', 'harbourline', 'Chartering manager'],
  ['niran-somchai', 'Niran Somchai', 'bangkok', 'harbourline', 'Agent'],
  ['hiro-tanaka', 'Hiro Tanaka', 'tokyo', 'harbourline', 'Agent'],
  ['ellen-brooks', 'Ellen Brooks', 'chicago', 'northgate', 'Chief operating officer'],
  ['marcus-hill', 'Marcus Hill', 'toronto', 'northgate', 'Terminal manager'],
  ['olivia-chen', 'Olivia Chen', 'vancouver', 'northgate', 'Terminal manager'],
  ['sam-ortiz', 'Sam Ortiz', 'losangeles', 'northgate', 'Intermodal lead'],
  ['valentina-ruiz', 'Valentina Ruiz', 'lima', 'andes', 'Managing director'],
  ['joaquin-pena', 'Joaquín Peña', 'santiago', 'andes', 'Operations lead'],
  ['sofia-martinez', 'Sofía Martínez', 'buenosaires', 'andes', 'Sales lead'],
  ['selin-aydin', 'Selin Aydın', 'istanbul', 'bosphorus', 'Managing director'],
  ['mehmet-demir', 'Mehmet Demir', 'istanbul', 'bosphorus', 'Cargo planner'],
  ['youssef-alami', 'Youssef Alami', 'casablanca', 'bosphorus', 'Station manager'],
  ['ruby-thompson', 'Ruby Thompson', 'melbourne', 'southerncross', 'Port director'],
  ['aroha-ngata', 'Aroha Ngata', 'auckland', 'southerncross', 'Terminal manager'],
  ['jack-nguyen', 'Jack Nguyen', 'sydney', 'southerncross', 'Planner'],
];
export const WORLD_PEOPLE = new Map(PEOPLE.map(([id, label]) => [id, label]));

/** Who knows whom (each link is stated one way). */
export const WORLD_KNOWS = [
  // the head office and the heads of the regions
  ['amira-haddad', 'omar-rahman'],
  ['amira-haddad', 'sara-lindgren'],
  ['amira-haddad', 'wei-lin-tan'],
  ['amira-haddad', 'grace-walker'],
  ['amira-haddad', 'thandi-nkosi'],
  ['amira-haddad', 'camila-souza'],
  ['amira-haddad', 'liam-carter'],
  ['omar-rahman', 'nadia-farouk'],
  ['omar-rahman', 'priya-nair'],
  ['omar-rahman', 'emre-kaya'],
  ['omar-rahman', 'wei-lin-tan'],
  ['priya-nair', 'arjun-mehta'],
  ['priya-nair', 'wei-lin-tan'],
  ['arjun-mehta', 'nadia-farouk'],
  ['nadia-farouk', 'wanjiru-mwangi'],
  // Europe and Türkiye
  ['sara-lindgren', 'tom-ashworth'],
  ['sara-lindgren', 'claire-dubois'],
  ['sara-lindgren', 'jonas-weber'],
  ['tom-ashworth', 'claire-dubois'],
  ['tom-ashworth', 'grace-walker'],
  ['jonas-weber', 'lucia-moreno'],
  ['jonas-weber', 'emre-kaya'],
  ['lucia-moreno', 'youssef-alami'],
  ['emre-kaya', 'selin-aydin'],
  ['emre-kaya', 'mehmet-demir'],
  ['selin-aydin', 'mehmet-demir'],
  ['selin-aydin', 'youssef-alami'],
  ['claire-dubois', 'tunde-adebayo'],
  // Africa
  ['thandi-nkosi', 'wanjiru-mwangi'],
  ['thandi-nkosi', 'tunde-adebayo'],
  ['tunde-adebayo', 'youssef-alami'],
  ['wanjiru-mwangi', 'priya-nair'],
  // Asia-Pacific
  ['wei-lin-tan', 'siti-rahma'],
  ['wei-lin-tan', 'chen-jie'],
  ['wei-lin-tan', 'kai-wong'],
  ['wei-lin-tan', 'yuki-sato'],
  ['wei-lin-tan', 'liam-carter'],
  ['siti-rahma', 'niran-somchai'],
  ['chen-jie', 'kai-wong'],
  ['chen-jie', 'mei-chan'],
  ['chen-jie', 'min-jun-park'],
  ['kai-wong', 'mei-chan'],
  ['kai-wong', 'niran-somchai'],
  ['mei-chan', 'niran-somchai'],
  ['min-jun-park', 'yuki-sato'],
  ['yuki-sato', 'hiro-tanaka'],
  ['min-jun-park', 'hiro-tanaka'],
  ['hiro-tanaka', 'sam-ortiz'],
  // Oceania
  ['liam-carter', 'ruby-thompson'],
  ['liam-carter', 'jack-nguyen'],
  ['ruby-thompson', 'aroha-ngata'],
  ['ruby-thompson', 'jack-nguyen'],
  ['jack-nguyen', 'aroha-ngata'],
  // North America
  ['grace-walker', 'ellen-brooks'],
  ['grace-walker', 'diego-alvarez'],
  ['grace-walker', 'marcus-hill'],
  ['ellen-brooks', 'marcus-hill'],
  ['ellen-brooks', 'olivia-chen'],
  ['ellen-brooks', 'sam-ortiz'],
  ['olivia-chen', 'sam-ortiz'],
  ['diego-alvarez', 'sam-ortiz'],
  ['diego-alvarez', 'mateo-rojas'],
  // Latin America
  ['camila-souza', 'sofia-martinez'],
  ['camila-souza', 'mateo-rojas'],
  ['camila-souza', 'valentina-ruiz'],
  ['valentina-ruiz', 'joaquin-pena'],
  ['valentina-ruiz', 'mateo-rojas'],
  ['joaquin-pena', 'sofia-martinez'],
];

/** from, to, flights a week */
const ROUTES = [
  ['dubai', 'london', 21],
  ['dubai', 'frankfurt', 14],
  ['dubai', 'istanbul', 10],
  ['dubai', 'cairo', 7],
  ['dubai', 'nairobi', 7],
  ['dubai', 'lagos', 5],
  ['dubai', 'johannesburg', 7],
  ['dubai', 'mumbai', 14],
  ['dubai', 'delhi', 10],
  ['dubai', 'singapore', 14],
  ['dubai', 'shanghai', 7],
  ['dubai', 'hongkong', 7],
  ['dubai', 'sydney', 5],
  ['dubai', 'newyork', 7],
  ['dubai', 'saopaulo', 4],
  ['singapore', 'jakarta', 10],
  ['singapore', 'bangkok', 7],
  ['singapore', 'sydney', 7],
  ['singapore', 'tokyo', 7],
  ['singapore', 'seoul', 5],
  ['hongkong', 'shanghai', 10],
  ['sydney', 'auckland', 7],
  ['sydney', 'melbourne', 14],
  ['london', 'newyork', 14],
  ['london', 'toronto', 5],
  ['london', 'madrid', 7],
  ['istanbul', 'casablanca', 5],
  ['istanbul', 'paris', 7],
  ['newyork', 'chicago', 10],
  ['newyork', 'mexicocity', 7],
  ['newyork', 'bogota', 4],
  ['chicago', 'losangeles', 7],
  ['chicago', 'vancouver', 5],
  ['saopaulo', 'buenosaires', 7],
  ['saopaulo', 'santiago', 5],
  ['saopaulo', 'lima', 5],
  ['lima', 'bogota', 4],
];

const point = (lon, lat) => `"POINT(${lon} ${lat})"^^geo:wktLiteral`;

/** The dataset as Turtle. */
export function worldTurtle() {
  let ttl = PREFIXES;
  for (const [id, label, lon, lat] of CITIES)
    ttl += `ex:${id} a ex:City ; rdfs:label "${label}"@en ; geo:hasDefaultGeometry ex:${id}Geom .
ex:${id}Geom geo:asWKT ${point(lon, lat)} .
`;
  for (const [id, label, hq] of COMPANIES) {
    const c = CITY.get(hq);
    ttl += `ex:${id} a ex:Company ; rdfs:label "${label}"@en ; ex:headquarters ex:${hq} ; geo:hasDefaultGeometry ex:${id}Geom .
ex:${id}Geom geo:asWKT ${point(c.lon, c.lat)} .
`;
  }
  for (const o of OFFICES) ttl += `ex:meridian ex:hasOffice ex:${o} .\n`;
  for (const [id] of COMPANIES.slice(1)) ttl += `ex:meridian ex:partnerOf ex:${id} .\n`;
  PEOPLE.forEach(([id, label, city, employer, role], i) => {
    const c = CITY.get(city);
    // a little apart from the city's own marker, and from a colleague in the same city
    const lon = +(c.lon + 0.02 + (i % 3) * 0.01).toFixed(4);
    const lat = +(c.lat + 0.01 + (i % 2) * 0.01).toFixed(4);
    ttl += `ex:${id} a ex:Person ; rdfs:label "${label}"@en ; ex:jobTitle "${role}"@en ;
  ex:livesIn ex:${city} ; ex:worksFor ex:${employer} ; geo:hasDefaultGeometry ex:${id}Geom .
ex:${id}Geom geo:asWKT ${point(lon, lat)} .
`;
  });
  for (const [a, b] of WORLD_KNOWS) ttl += `ex:${a} ex:knows ex:${b} .\n`;
  for (const [from, to, weekly] of ROUTES) {
    const f = CITY.get(from);
    const t = CITY.get(to);
    ttl += `ex:route-${from}-${to} a ex:Route ; rdfs:label "${f.label} to ${t.label}" ;
  ex:from ex:${from} ; ex:to ex:${to} ;
  ex:weeklyFlights "${weekly}"^^xsd:integer ;
  ex:fromGeom ${point(f.lon, f.lat)} ; ex:toGeom ${point(t.lon, t.lat)} .
`;
  }
  return ttl;
}

const C = (n) => `urn:x-sparkles:graphrag:world:${n}`;
const KNOWS = `${WORLD}knows`;
const WORKS_FOR = `${WORLD}worksFor`;

/**
 * The community tree: key, title, summary, rating, status, members, children and the
 * findings of its report, each with the people it is about and the triples it cites.
 */
const TREE = [
  {
    key: 'apac',
    title: 'Asia-Pacific freight network',
    summary:
      'The Singapore hub and the Harbour Line partnership move most of the cargo between China, Southeast Asia and North Asia.',
    rating: 9,
    status: 'reviewed',
    findings: [
      [
        'Wei Lin Tan links the Singapore hub with Shanghai, Hong Kong and Tokyo.',
        ['wei-lin-tan', 'chen-jie'],
        [
          ['wei-lin-tan', KNOWS, 'chen-jie'],
          ['wei-lin-tan', KNOWS, 'kai-wong'],
        ],
      ],
      [
        'Harbour Line Shipping carries the Hong Kong to Shanghai leg for Meridian.',
        ['kai-wong', 'chen-jie'],
        [['chen-jie', KNOWS, 'kai-wong']],
      ],
      [
        'Hiro Tanaka is the only contact across the Pacific, to the Northgate team in Los Angeles.',
        ['hiro-tanaka', 'sam-ortiz'],
        [['hiro-tanaka', KNOWS, 'sam-ortiz']],
      ],
    ],
    children: [
      {
        key: 'harbour',
        title: 'Harbour Line partnership',
        summary: 'Kai Wong and Mei Chan run the charters that Chen Jie books from Shanghai.',
        rating: 7,
        status: 'unreviewed',
        findings: [
          [
            'Kai Wong, Mei Chan and Niran Somchai all know each other.',
            ['kai-wong', 'mei-chan', 'niran-somchai'],
            [
              ['kai-wong', KNOWS, 'mei-chan'],
              ['mei-chan', KNOWS, 'niran-somchai'],
            ],
          ],
        ],
        children: [
          {
            key: 'hk',
            title: 'Hong Kong chartering desk',
            summary: 'The fleet and chartering managers of Harbour Line Shipping.',
            rating: 5,
            status: 'unreviewed',
            members: ['kai-wong', 'mei-chan'],
            findings: [
              [
                'Both work for Harbour Line Shipping in Hong Kong.',
                ['kai-wong', 'mei-chan'],
                [['kai-wong', WORKS_FOR, 'harbourline']],
              ],
            ],
          },
          {
            key: 'gateways',
            title: 'Shanghai and Bangkok gateways',
            summary: 'The hub manager in Shanghai and Harbour Line’s agent in Bangkok.',
            rating: 4,
            status: 'stale',
            members: ['chen-jie', 'niran-somchai'],
            findings: [
              [
                'Chen Jie reaches Bangkok only through Harbour Line.',
                ['chen-jie', 'niran-somchai'],
                [['chen-jie', KNOWS, 'mei-chan']],
              ],
            ],
          },
        ],
      },
      {
        key: 'northasia',
        title: 'North Asia desk',
        summary: 'Yuki Sato leads the accounts in Tokyo and Seoul.',
        rating: 6,
        status: 'reviewed',
        members: ['yuki-sato', 'min-jun-park', 'hiro-tanaka'],
        findings: [
          [
            'Yuki Sato, Min-jun Park and Hiro Tanaka form a closed triangle.',
            ['yuki-sato', 'min-jun-park', 'hiro-tanaka'],
            [
              ['min-jun-park', KNOWS, 'yuki-sato'],
              ['yuki-sato', KNOWS, 'hiro-tanaka'],
            ],
          ],
        ],
      },
      {
        key: 'sg',
        title: 'Singapore hub',
        summary: 'The regional head office and the Jakarta sales team.',
        rating: 6,
        status: 'unreviewed',
        members: ['wei-lin-tan', 'siti-rahma'],
        findings: [
          [
            'Siti Rahma reports to Wei Lin Tan.',
            ['siti-rahma', 'wei-lin-tan'],
            [['wei-lin-tan', KNOWS, 'siti-rahma']],
          ],
        ],
      },
    ],
  },
  {
    key: 'europe',
    title: 'Europe and the Bosphorus air bridge',
    summary:
      'London leads the European offices, and Istanbul connects them by air with Casablanca and the Gulf.',
    rating: 8,
    status: 'reviewed',
    findings: [
      [
        'Sara Lindgren is the link between the European offices and the head office.',
        ['sara-lindgren', 'amira-haddad'],
        [['amira-haddad', KNOWS, 'sara-lindgren']],
      ],
      [
        'Bosphorus Air Cargo flies the Istanbul to Paris and Istanbul to Casablanca routes.',
        ['selin-aydin', 'emre-kaya'],
        [['emre-kaya', KNOWS, 'selin-aydin']],
      ],
    ],
    children: [
      {
        key: 'lonpar',
        title: 'London and Paris accounts',
        summary: 'Account management and compliance for the European customers.',
        rating: 6,
        status: 'reviewed',
        members: ['sara-lindgren', 'tom-ashworth', 'claire-dubois'],
        findings: [
          [
            'Tom Ashworth also looks after the New York accounts.',
            ['tom-ashworth', 'grace-walker'],
            [['tom-ashworth', KNOWS, 'grace-walker']],
          ],
        ],
      },
      {
        key: 'continent',
        title: 'Frankfurt and Madrid hubs',
        summary: 'The continental hub in Frankfurt and the route planning in Madrid.',
        rating: 5,
        status: 'unreviewed',
        members: ['jonas-weber', 'lucia-moreno'],
        findings: [
          [
            'Lucía Moreno plans the routes into North Africa with Youssef Alami.',
            ['lucia-moreno', 'youssef-alami'],
            [['lucia-moreno', KNOWS, 'youssef-alami']],
          ],
        ],
      },
      {
        key: 'bosphorus',
        title: 'Bosphorus air bridge',
        summary: 'Meridian’s air freight lead and the Bosphorus Air Cargo team in Istanbul.',
        rating: 7,
        status: 'rejected',
        members: ['emre-kaya', 'selin-aydin', 'mehmet-demir'],
        findings: [
          [
            'Emre Kaya, Selin Aydın and Mehmet Demir plan the air bridge together.',
            ['emre-kaya', 'selin-aydin', 'mehmet-demir'],
            [['emre-kaya', KNOWS, 'mehmet-demir']],
          ],
        ],
      },
    ],
  },
  {
    key: 'americas',
    title: 'North American rail corridor',
    summary: 'Grace Walker in New York works with Northgate Rail from Toronto to Los Angeles.',
    rating: 7,
    status: 'unreviewed',
    findings: [
      [
        'Northgate Rail carries Meridian’s containers inland from New York.',
        ['ellen-brooks', 'grace-walker'],
        [['grace-walker', KNOWS, 'ellen-brooks']],
      ],
      [
        'Diego Álvarez connects Mexico City with Bogotá.',
        ['diego-alvarez', 'mateo-rojas'],
        [['diego-alvarez', KNOWS, 'mateo-rojas']],
      ],
    ],
    children: [
      {
        key: 'northgate',
        title: 'Northgate corridor',
        summary: 'The Chicago operations and the terminals on both coasts.',
        rating: 6,
        status: 'unreviewed',
        findings: [
          [
            'Ellen Brooks knows every terminal manager of Northgate Rail.',
            ['ellen-brooks'],
            [
              ['ellen-brooks', KNOWS, 'marcus-hill'],
              ['ellen-brooks', KNOWS, 'olivia-chen'],
            ],
          ],
        ],
        children: [
          {
            key: 'lakes',
            title: 'Great Lakes terminals',
            summary: 'Chicago and Toronto.',
            rating: 4,
            status: 'unreviewed',
            members: ['ellen-brooks', 'marcus-hill'],
            findings: [
              [
                'Marcus Hill runs the Toronto terminal.',
                ['marcus-hill'],
                [['marcus-hill', WORKS_FOR, 'northgate']],
              ],
            ],
          },
          {
            key: 'westcoast',
            title: 'West coast terminals',
            summary: 'Vancouver and Los Angeles.',
            rating: 4,
            status: 'reviewed',
            members: ['olivia-chen', 'sam-ortiz'],
            findings: [
              [
                'Olivia Chen and Sam Ortiz share the transpacific traffic.',
                ['olivia-chen', 'sam-ortiz'],
                [['olivia-chen', KNOWS, 'sam-ortiz']],
              ],
            ],
          },
        ],
      },
      {
        key: 'nymx',
        title: 'New York and Mexico City',
        summary: 'Meridian’s own offices in the Americas.',
        rating: 5,
        status: 'stale',
        members: ['grace-walker', 'diego-alvarez'],
        findings: [
          [
            'Grace Walker and Diego Álvarez run the New York to Mexico City route.',
            ['grace-walker', 'diego-alvarez'],
            [['grace-walker', KNOWS, 'diego-alvarez']],
          ],
        ],
      },
    ],
  },
  {
    key: 'gulf',
    title: 'Gulf head office',
    summary: 'The Dubai leadership with the customs and Indian port teams it works with daily.',
    rating: 8,
    status: 'reviewed',
    findings: [
      [
        'Amira Haddad knows the head of every region.',
        ['amira-haddad'],
        [
          ['amira-haddad', KNOWS, 'wei-lin-tan'],
          ['amira-haddad', KNOWS, 'grace-walker'],
          ['amira-haddad', KNOWS, 'thandi-nkosi'],
        ],
      ],
      [
        'Omar Rahman is the second link to Asia, through Priya Nair and Wei Lin Tan.',
        ['omar-rahman', 'priya-nair'],
        [
          ['omar-rahman', KNOWS, 'priya-nair'],
          ['omar-rahman', KNOWS, 'wei-lin-tan'],
        ],
      ],
    ],
    children: [
      {
        key: 'dubai',
        title: 'Dubai leadership',
        summary: 'The chief executive, the head of operations and the customs lead.',
        rating: 7,
        status: 'reviewed',
        members: ['amira-haddad', 'omar-rahman', 'nadia-farouk'],
        findings: [
          [
            'Nadia Farouk clears the Cairo and Nairobi traffic for the head office.',
            ['nadia-farouk', 'omar-rahman'],
            [['omar-rahman', KNOWS, 'nadia-farouk']],
          ],
        ],
      },
      {
        key: 'india',
        title: 'India operations',
        summary: 'The Mumbai port and the network planning in Delhi.',
        rating: 5,
        status: 'unreviewed',
        members: ['priya-nair', 'arjun-mehta'],
        findings: [
          [
            'Priya Nair and Arjun Mehta plan the Mumbai and Delhi rotations.',
            ['priya-nair', 'arjun-mehta'],
            [['priya-nair', KNOWS, 'arjun-mehta']],
          ],
        ],
      },
    ],
  },
  {
    key: 'latam',
    title: 'Latin America and Andes Cargo',
    summary: 'São Paulo and the Andes Cargo partnership cover the routes south of Bogotá.',
    rating: 6,
    status: 'unreviewed',
    findings: [
      [
        'Camila Souza works with Andes Cargo through Valentina Ruiz.',
        ['camila-souza', 'valentina-ruiz'],
        [['camila-souza', KNOWS, 'valentina-ruiz']],
      ],
    ],
    children: [
      {
        key: 'andes',
        title: 'Andes Cargo partnership',
        summary: 'Lima, Santiago and the Bogotá route analyst.',
        rating: 5,
        status: 'unreviewed',
        members: ['valentina-ruiz', 'joaquin-pena', 'mateo-rojas'],
        findings: [
          [
            'Valentina Ruiz leads the partnership from Lima.',
            ['valentina-ruiz'],
            [['valentina-ruiz', KNOWS, 'joaquin-pena']],
          ],
        ],
      },
      {
        key: 'cone',
        title: 'Southern Cone sales',
        summary: 'São Paulo and Buenos Aires.',
        rating: 4,
        status: 'reviewed',
        members: ['camila-souza', 'sofia-martinez'],
        findings: [
          [
            'Sofía Martínez sells the Buenos Aires route.',
            ['sofia-martinez'],
            [['camila-souza', KNOWS, 'sofia-martinez']],
          ],
        ],
      },
    ],
  },
  {
    key: 'africa',
    title: 'Africa routes',
    summary: 'Johannesburg leads the African offices, from Lagos and Casablanca to Nairobi.',
    rating: 6,
    status: 'stale',
    findings: [
      [
        'Thandi Nkosi knows the managers in Nairobi and Lagos.',
        ['thandi-nkosi'],
        [
          ['thandi-nkosi', KNOWS, 'wanjiru-mwangi'],
          ['thandi-nkosi', KNOWS, 'tunde-adebayo'],
        ],
      ],
    ],
    children: [
      {
        key: 'westafrica',
        title: 'West and North Africa',
        summary: 'Lagos and Casablanca.',
        rating: 4,
        status: 'unreviewed',
        members: ['tunde-adebayo', 'youssef-alami'],
        findings: [
          [
            'Tunde Adebayo and Youssef Alami share the Lagos air freight.',
            ['tunde-adebayo', 'youssef-alami'],
            [['tunde-adebayo', KNOWS, 'youssef-alami']],
          ],
        ],
      },
      {
        key: 'eastafrica',
        title: 'East and Southern Africa',
        summary: 'Johannesburg and Nairobi.',
        rating: 5,
        status: 'reviewed',
        members: ['thandi-nkosi', 'wanjiru-mwangi'],
        findings: [
          [
            'Wanjiru Mwangi also works with the Mumbai port.',
            ['wanjiru-mwangi', 'priya-nair'],
            [['wanjiru-mwangi', KNOWS, 'priya-nair']],
          ],
        ],
      },
    ],
  },
  {
    key: 'oceania',
    title: 'Oceania ports',
    summary: 'Southern Cross Ports and Meridian’s Sydney office handle Australia and New Zealand.',
    rating: 5,
    status: 'unreviewed',
    members: ['liam-carter', 'ruby-thompson', 'aroha-ngata', 'jack-nguyen'],
    findings: [
      [
        'Liam Carter is the only member who knows anyone in Singapore.',
        ['liam-carter', 'wei-lin-tan'],
        [['wei-lin-tan', KNOWS, 'liam-carter']],
      ],
      [
        'Ruby Thompson, Aroha Ngata and Jack Nguyen all know each other.',
        ['ruby-thompson', 'aroha-ngata', 'jack-nguyen'],
        [
          ['ruby-thompson', KNOWS, 'aroha-ngata'],
          ['jack-nguyen', KNOWS, 'aroha-ngata'],
        ],
      ],
    ],
  },
];

/** The communities of `world` in the shape mock/graphrag.mjs keeps, keyed by IRI. */
export function worldCommunities() {
  const out = {};
  let n = 0;
  const iri = (t) => `${WORLD}${t}`;
  const walk = (node, level, parent) => {
    const id = C(`${level}:${node.key}`);
    const kids = (node.children ?? []).map((k) => walk(k, level + 1, id));
    const members = node.members ?? [...new Set(kids.flatMap((k) => out[k].members))];
    out[id] = {
      iri: id,
      level,
      parent,
      members,
      children: kids,
      report: {
        iri: `urn:uuid:world-${++n}`,
        title: node.title,
        summary: node.summary,
        status: node.status,
        rating: node.rating,
        failure: null,
        revisionOf: null,
        droppedCitations: 0,
        droppedUnsupported: 0,
        sourceCommit: 1,
        activity: {
          model: 'mock/small',
          inputTokens: 900 + members.length * 260,
          outputTokens: 120 + node.findings.length * 70,
          startedAt: '2026-10-20T09:00:00Z',
        },
        findings: node.findings.map(([text, about, cites]) => ({
          text,
          about: about.map(iri),
          cites: cites.map(([s, p, o]) => `<<( <${iri(s)}> <${p}> <${iri(o)}> )>>`),
          unmatchedNames: [],
        })),
      },
    };
    return id;
  };
  for (const node of TREE) walk(node, 0, null);
  return out;
}
